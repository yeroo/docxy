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
    Tab,
    /// A SmartArt / DrawingML diagram. `raw` is the original run XML, preserved
    /// verbatim for lossless save; `text` is the diagram's node text, extracted
    /// from the external diagram part at load time. The terminal can't draw the
    /// diagram's shapes, so the renderer shows this text in a box instead.
    SmartArt {
        raw: String,
        text: Vec<String>,
    },
    /// A legacy Equation Editor (`Equation.3`) object decoded to Unicode math
    /// text. `raw` is the original run XML (preserved verbatim for lossless save);
    /// `text` is the decoded equation, rendered as inline text at body size.
    Equation {
        raw: String,
        text: String,
    },
    /// A text box / shape with text (`<w:txbxContent>`). `blocks` is its editable
    /// content (addressable by path, so the caret can enter it); `raw` is the
    /// original run XML, whose `txbxContent` is replaced with the serialized
    /// `blocks` on save so the surrounding shape is preserved.
    TextBox {
        raw: String,
        blocks: Vec<Block>,
    },
    /// Verbatim XML for inline content we don't model (images/fields/bookmarks),
    /// preserved so save stays lossless. Zero-length and invisible for now.
    Raw(String),
}

impl Inline {
    /// The visible text this inline contributes (tabs/breaks as whitespace).
    pub fn text(&self) -> String {
        match self {
            Inline::Run(r) => r.text.clone(),
            Inline::Hyperlink(h) => h.runs.iter().map(|r| r.text.as_str()).collect(),
            Inline::Tab => "\t".to_string(),
            Inline::Break(_) => "\n".to_string(),
            Inline::SmartArt { text, .. } => text.join("\n"),
            Inline::Equation { text, .. } => text.clone(),
            Inline::TextBox { blocks, .. } => blocks
                .iter()
                .map(|b| b.plain_text())
                .collect::<Vec<_>>()
                .join("\n"),
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
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Column widths in twips (`w:tblGrid`/`w:gridCol`).
    pub grid: Vec<u32>,
    pub rows: Vec<Row>,
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
