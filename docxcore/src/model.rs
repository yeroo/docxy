//! The editable document model (a simplified, render-ready OOXML AST).
//!
//! This is intentionally a *semantic* tree, not a faithful XML mirror: it keeps
//! what the terminal renderer and the PDF exporter need (text, run/paragraph
//! properties, tables, lists, hyperlinks). Lossless round-trip preservation of
//! unmodeled parts is a separate concern handled at save time (a later phase).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

/// Character-level formatting (a resolved `w:rPr`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunProps {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Inline code / monospace (`<w:rStyle w:val="Code"/>`). Markdown `` `x` ``.
    pub code: bool,
    pub caps: bool,
    pub small_caps: bool,
    /// Hidden text (`w:vanish` / `w:webHidden`).
    pub vanish: bool,
    pub vert_align: VertAlign,
    /// Hex `RRGGBB` (uppercased), if an explicit non-auto color was set.
    pub color: Option<String>,
    /// Highlight color name (e.g. `yellow`).
    pub highlight: Option<String>,
    /// Font size in half-points (`w:sz`). Ignored by the TUI, used by PDF.
    pub size_half_pts: Option<u32>,
    /// ASCII font family (`w:rFonts w:ascii`). Ignored by the TUI, used by PDF.
    pub font: Option<String>,
    /// Character style id (`w:rStyle`).
    pub style_id: Option<String>,
    /// Verbatim XML of `w:rPr` children we don't model (character spacing
    /// `w:spacing`/`w:kern`, `w:lang`, `w:shd`, `w:effect`, …), preserved so save
    /// doesn't drop them. Re-emitted at the end of `w:rPr`.
    pub raw_props: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub props: RunProps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    /// Resolved target URL (external link) if present.
    pub target: Option<String>,
    /// In-document anchor (`w:anchor`) if present.
    pub anchor: Option<String>,
    /// Original relationship id (`r:id`), preserved so save can write it back
    /// unchanged (the `.rels` part itself is preserved verbatim).
    pub rel_id: Option<String>,
    pub runs: Vec<Run>,
}

/// The kind of an in-line break (`w:br`/`w:cr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakKind {
    #[default]
    Line,
    Page,
    Column,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Run(Run),
    Hyperlink(Hyperlink),
    Break(BreakKind),
    /// A tab character. Carries the run properties of the run it came from, so an
    /// underlined tab (the common "type a line across the footer" trick) keeps its
    /// underline both on screen and on save.
    Tab(RunProps),
    /// A SmartArt / DrawingML diagram. `raw` is the original run XML, preserved
    /// verbatim for lossless save; `text` is the diagram's node text, extracted
    /// from the external diagram part at load time. The terminal can't draw the
    /// diagram's shapes, so the renderer shows this text in a box instead.
    SmartArt {
        raw: String,
        text: Vec<String>,
    },
    /// A DrawingML chart. `raw` is the original run XML (preserved verbatim for
    /// lossless save); `chart` is the parsed plot data, rendered as a text
    /// bar/pie view in a box since a terminal can't draw chart graphics.
    Chart {
        raw: String,
        chart: crate::chart::Chart,
    },
    /// A math equation. `raw` is the original XML preserved verbatim for lossless
    /// save: OMML (`<m:oMath>`/`<m:oMathPara>`) for native Word math, or a legacy
    /// Equation Editor (`Equation.3`) object's run XML. `text` is the equation
    /// rendered to Unicode, shown inline at body size. `latex` is the LaTeX source
    /// when known (Markdown-authored math, or derived from OMML) — `None` for
    /// legacy objects; it lets `$…$` round-trip exactly through Markdown.
    Equation {
        raw: String,
        text: String,
        latex: Option<String>,
    },
    /// A text box / shape with text (`<w:txbxContent>`). `blocks` is its editable
    /// content (addressable by path, so the caret can enter it); `raw` is the
    /// original run XML, whose `txbxContent` is replaced with the serialized
    /// `blocks` on save so the surrounding shape is preserved.
    TextBox {
        raw: String,
        blocks: Vec<Block>,
    },
    /// A field (`<w:fldSimple>`, e.g. CREATEDATE/PAGE/REF). `raw` is the original
    /// XML (preserved verbatim for lossless save); `text` is the field's cached
    /// result, rendered as inline body text so the value (a date, a number, …) is
    /// visible instead of vanishing.
    Field {
        raw: String,
        text: String,
    },
    /// A tracked change: `<w:ins>` (insertion) or `<w:del>` (deletion). `raw` is
    /// the original element preserved verbatim for lossless save; `content` is the
    /// inner inline content with a display style baked in (deletions struck
    /// through) so it renders visibly instead of vanishing into opaque `Raw`.
    Revision {
        kind: RevisionKind,
        raw: String,
        content: Vec<Inline>,
    },
    /// A footnote / endnote reference (`<w:footnoteReference>` /
    /// `<w:endnoteReference>`). `id` is the note id (also its display number for
    /// normal documents, whose notes are numbered 1, 2, 3…); `raw` is the whole
    /// reference run, preserved verbatim so save keeps the anchor (otherwise the
    /// notes part is orphaned). Rendered as a superscript marker; the note body
    /// lives in `word/footnotes.xml` / `endnotes.xml` (see [`crate::notes`]).
    FootnoteRef {
        id: i32,
        endnote: bool,
        raw: String,
    },
    /// Verbatim XML for inline content we don't model (images/bookmarks),
    /// preserved so save stays lossless. Zero-length and invisible for now.
    Raw(String),
}

/// The kind of a tracked change ([`Inline::Revision`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionKind {
    /// `<w:ins>` — inserted content.
    Insert,
    /// `<w:del>` — deleted content (shown struck through).
    Delete,
}

impl Inline {
    /// The visible text this inline contributes (tabs/breaks as whitespace).
    pub fn text(&self) -> String {
        match self {
            Inline::Run(r) => r.text.clone(),
            Inline::Hyperlink(h) => h.runs.iter().map(|r| r.text.as_str()).collect(),
            Inline::Tab(_) => "\t".to_string(),
            Inline::Break(_) => "\n".to_string(),
            Inline::SmartArt { text, .. } => text.join("\n"),
            Inline::Chart { chart, .. } => chart.title.clone().unwrap_or_default(),
            Inline::Equation { text, .. } => text.clone(),
            Inline::Field { text, .. } => text.clone(),
            Inline::TextBox { blocks, .. } => blocks
                .iter()
                .map(|b| b.plain_text())
                .collect::<Vec<_>>()
                .join("\n"),
            Inline::Revision { content, .. } => content.iter().map(|i| i.text()).collect(),
            Inline::FootnoteRef { id, .. } => id.to_string(),
            Inline::Raw(_) => String::new(),
        }
    }
}

/// Paragraph-level formatting (a resolved `w:pPr`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParProps {
    pub style_id: Option<String>,
    pub align: Align,
    /// Heading level 1..=9 if this paragraph is a heading (resolved at load).
    pub heading_level: Option<u8>,
    /// List membership: numbering id (`w:numId`) and level (`w:ilvl`).
    pub num_id: Option<i32>,
    pub ilvl: i32,
    pub rtl: bool,
    /// Legacy text-frame positioning (`w:framePr`): floats the paragraph to an
    /// absolute page/margin position. Present only for "floating" content.
    pub frame: Option<FramePr>,
    /// A section break: the verbatim `<w:sectPr>` XML carried in this paragraph's
    /// `pPr`, describing the section that **ends** here (page size/orientation/
    /// margins/headers). Preserved on save and used for per-section print layout.
    pub section_break: Option<String>,
    /// Direct tab stops (`w:tabs`), which override the paragraph style's.
    pub tabs: Vec<TabStop>,
    /// Direct paragraph borders (`w:pBdr`). A bottom (or top) border renders as a
    /// horizontal rule — Word's "horizontal line".
    pub borders: ParBorders,
    /// Left indent in twips (`w:ind w:left`/`w:start`). Rendered as leading space.
    pub indent: i32,
    /// Extra indent on the paragraph's first line, in twips, relative to `indent`:
    /// positive = `w:firstLine` (first line indented more), negative = `w:hanging`
    /// (first line pulled left of the rest, as in lists/bibliographies). Zero =
    /// every line shares `indent`.
    pub first_line: i32,
    /// Verbatim XML of `w:pPr` children we don't model (shading `w:shd`, spacing
    /// `w:spacing`, `w:keepNext`, `w:outlineLvl`, …), preserved so save doesn't
    /// silently drop them. Re-emitted in `w:pPr` in document order.
    pub raw_props: Vec<String>,
}

/// Paragraph borders (`w:pBdr`). Only the horizontal sides are modeled, since
/// that's what reads as a rule in a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParBorders {
    pub top: Option<BorderKind>,
    pub bottom: Option<BorderKind>,
}

/// A border line style (`w:val` on a `w:pBdr` side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderKind {
    Single,
    Double,
    Thick,
    Dotted,
    Dashed,
    Wavy,
}

impl BorderKind {
    /// Map a `w:val` to a kind; `None` for an absent/`nil`/`none` border.
    pub fn from_val(val: &str) -> Option<BorderKind> {
        match val {
            "single" => Some(BorderKind::Single),
            "double" => Some(BorderKind::Double),
            "thick" | "triple" => Some(BorderKind::Thick),
            "dotted" | "dotDash" | "dotDotDash" => Some(BorderKind::Dotted),
            "dashed" | "dashSmallGap" | "dashDotStroked" => Some(BorderKind::Dashed),
            "wave" | "doubleWave" => Some(BorderKind::Wavy),
            _ => None,
        }
    }
    /// The `w:val` written back on save.
    pub fn to_val(self) -> &'static str {
        match self {
            BorderKind::Single => "single",
            BorderKind::Double => "double",
            BorderKind::Thick => "thick",
            BorderKind::Dotted => "dotted",
            BorderKind::Dashed => "dashed",
            BorderKind::Wavy => "wave",
        }
    }
    /// The glyph used to draw the rule.
    pub fn glyph(self) -> char {
        match self {
            BorderKind::Single => '─',
            BorderKind::Double => '═',
            BorderKind::Thick => '━',
            BorderKind::Dotted => '┈',
            BorderKind::Dashed => '╌',
            BorderKind::Wavy => '∿',
        }
    }
}

/// A `w:framePr` text frame: absolute placement in twips (1/1440 inch).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FramePr {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub w: Option<i32>,
    pub h: Option<i32>,
    /// `page` | `margin` | `text` | `column` — what `x` is measured from.
    pub h_anchor: Option<String>,
    /// `page` | `margin` | `text` — what `y` is measured from.
    pub v_anchor: Option<String>,
    /// Keyword horizontal placement (`left|center|right|inside|outside`),
    /// used instead of `x`.
    pub x_align: Option<String>,
    /// Keyword vertical placement (`top|center|bottom|inside|outside|inline`),
    /// used instead of `y`.
    pub y_align: Option<String>,
}

/// A tab stop alignment (`w:tab w:val`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A tab stop leader fill (`w:tab w:leader`), e.g. the dots in a table of contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabLeader {
    #[default]
    None,
    Dot,
    Hyphen,
    Underscore,
}

/// A paragraph tab stop (`w:tab`): position in twips, alignment, and leader fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabStop {
    pub pos: i32,
    pub align: TabAlign,
    pub leader: TabLeader,
}

/// Page geometry from `w:sectPr` (`pgSz`/`pgMar`), in twips. Used to project
/// frame-positioned content onto the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageGeom {
    pub w: i32,
    pub h: i32,
    pub ml: i32,
    pub mr: i32,
    pub mt: i32,
    pub mb: i32,
    /// Number of newspaper columns in the section (`w:cols w:num`); 1 = single.
    pub cols: i32,
    /// Space between columns, in twips (`w:cols w:space`).
    pub col_space: i32,
}

impl Default for PageGeom {
    /// US Letter with 1" margins.
    fn default() -> Self {
        PageGeom {
            w: 12240,
            h: 15840,
            ml: 1440,
            mr: 1440,
            mt: 1440,
            mb: 1440,
            cols: 1,
            col_space: 720,
        }
    }
}

impl PageGeom {
    /// Parse page size/margins from a `<w:sectPr>` XML string (US Letter default
    /// for anything absent). `pgSz w:w/w:h` already hold the physical dimensions,
    /// so landscape sections need no special handling.
    pub fn from_sect_pr(sect: &str) -> PageGeom {
        let d = PageGeom::default();
        let attr = |tag: &str, key: &str, fallback: i32| -> i32 {
            (|| {
                let ts = sect.find(tag)?;
                let end = sect[ts..].find('>').map(|e| ts + e)?;
                let el = &sect[ts..end];
                let k = format!("{key}=\"");
                let ks = el.find(&k)? + k.len();
                let rest = &el[ks..];
                let e = rest.find('"')?;
                rest[..e].parse::<i32>().ok()
            })()
            .unwrap_or(fallback)
        };
        PageGeom {
            w: attr("<w:pgSz", "w:w", d.w),
            h: attr("<w:pgSz", "w:h", d.h),
            ml: attr("<w:pgMar", "w:left", d.ml),
            mr: attr("<w:pgMar", "w:right", d.mr),
            mt: attr("<w:pgMar", "w:top", d.mt),
            mb: attr("<w:pgMar", "w:bottom", d.mb),
            cols: attr("<w:cols", "w:num", d.cols).max(1),
            col_space: attr("<w:cols", "w:space", d.col_space).max(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paragraph {
    pub props: ParProps,
    pub content: Vec<Inline>,
}

impl Paragraph {
    pub fn plain_text(&self) -> String {
        self.content.iter().map(|i| i.text()).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VMerge {
    #[default]
    None,
    /// Top cell of a vertical merge.
    Restart,
    /// A cell merged into the one above.
    Continue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Horizontal span (`w:gridSpan`), >= 1.
    pub grid_span: u32,
    pub v_merge: VMerge,
    pub blocks: Vec<Block>,
    /// The cell's entire `w:tcPr` verbatim (borders, shading, width, vAlign, …),
    /// preserved so save round-trips cell formatting. `grid_span`/`v_merge` are
    /// also parsed out of it for rendering; when present it is re-emitted as-is
    /// instead of regenerating tcPr from the model. `None` for a new cell.
    pub raw_tcpr: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: Vec::new(),
            raw_tcpr: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Verbatim `w:trPr` / `w:tblPrEx` XML (row height, header flag, exceptions),
    /// preserved so save doesn't drop row formatting. Re-emitted in document order.
    pub raw_props: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Column widths in twips (`w:tblGrid`/`w:gridCol`).
    pub grid: Vec<u32>,
    pub rows: Vec<Row>,
    /// The table's entire `w:tblPr` verbatim (borders, shading, width, style,
    /// look, layout), preserved so save round-trips table formatting.
    pub raw_tblpr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    /// Verbatim XML for block-level content we don't model (content controls,
    /// etc.), preserved for lossless save.
    Raw(String),
}

impl Block {
    pub fn plain_text(&self) -> String {
        match self {
            Block::Raw(_) => String::new(),
            Block::Paragraph(p) => p.plain_text(),
            Block::Table(t) => {
                let mut s = String::new();
                for row in &t.rows {
                    let cells: Vec<String> = row
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
                    s.push_str(&cells.join("\t"));
                    s.push('\n');
                }
                s
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub body: Vec<Block>,
}

impl Document {
    /// Concatenated visible text, one block per line — handy for tests/sanity.
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for b in &self.body {
            s.push_str(&b.plain_text());
            if !matches!(b, Block::Table(_)) {
                s.push('\n');
            }
        }
        s
    }
}
