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
    /// Right indent in twips (`w:ind w:right`/`w:end`). Pulls the paragraph's right
    /// edge in from the right margin.
    pub indent_right: i32,
    /// Paragraph spacing (`w:spacing`): line spacing plus space before/after.
    /// All-`None` means no `w:spacing` element is emitted.
    pub spacing: Spacing,
    /// Verbatim XML of `w:pPr` children we don't model (shading `w:shd`,
    /// `w:keepNext`, `w:outlineLvl`, …), preserved so save doesn't
    /// silently drop them. Re-emitted in `w:pPr` in document order.
    pub raw_props: Vec<String>,
}

/// Paragraph spacing (`w:spacing`). Every CT_Spacing attribute is modeled so a
/// paragraph's spacing round-trips losslessly while `line`/`line_rule` stay
/// editable. Absent attributes are `None`; an all-`None` `Spacing` emits nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spacing {
    /// Space before the paragraph, in twips (`w:before`).
    pub before: Option<i32>,
    /// Space after the paragraph, in twips (`w:after`).
    pub after: Option<i32>,
    /// Space before, in hundredths of a line (`w:beforeLines`).
    pub before_lines: Option<i32>,
    /// Space after, in hundredths of a line (`w:afterLines`).
    pub after_lines: Option<i32>,
    /// `w:beforeAutospacing` (ST_OnOff) — kept as its raw token for losslessness.
    pub before_auto: Option<String>,
    /// `w:afterAutospacing` (ST_OnOff) — kept as its raw token.
    pub after_auto: Option<String>,
    /// Line spacing value (`w:line`). Meaning depends on `line_rule`: in 240ths of
    /// a line for `auto` (240 = single, 360 = 1.5×, 480 = double), or twips for
    /// `exact`/`atLeast`.
    pub line: Option<i32>,
    /// `w:lineRule`: `"auto"`, `"exact"`, or `"atLeast"`.
    pub line_rule: Option<String>,
}

impl Spacing {
    /// No spacing attributes set — no `w:spacing` element is written.
    pub fn is_empty(&self) -> bool {
        self.before.is_none()
            && self.after.is_none()
            && self.before_lines.is_none()
            && self.after_lines.is_none()
            && self.before_auto.is_none()
            && self.after_auto.is_none()
            && self.line.is_none()
            && self.line_rule.is_none()
    }

    /// The line-spacing multiple (e.g. 1.0, 1.5, 2.0) when `line_rule` is `auto`
    /// (or absent, which Word treats as `auto`). `None` for `exact`/`atLeast`.
    pub fn line_multiple(&self) -> Option<f32> {
        let auto = self.line_rule.as_deref().is_none_or(|r| r == "auto");
        if !auto {
            return None;
        }
        self.line.map(|l| l as f32 / 240.0)
    }
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
    /// The section declares decorative page borders (`w:pgBorders`), drawn in the
    /// page view as a double-line frame.
    pub page_border: bool,
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
            page_border: false,
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
            page_border: sect.contains("<w:pgBorders"),
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

/// An invisible table child anchored at the gap before `rows[at]` (or after the
/// last row when `at == rows.len()`). Boundary order is significant when
/// several children share a gap: all of them are applied in vector order before
/// the row at that position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRowBoundary {
    pub at: usize,
    pub kind: TableRowBoundaryKind,
}

impl TableRowBoundary {
    pub fn sdt_open(at: usize, raw: impl Into<String>) -> Self {
        Self {
            at,
            kind: TableRowBoundaryKind::SdtOpen(raw.into()),
        }
    }

    pub fn sdt_close(at: usize, raw: impl Into<String>) -> Self {
        Self {
            at,
            kind: TableRowBoundaryKind::SdtClose(raw.into()),
        }
    }

    pub fn raw(at: usize, raw: impl Into<String>) -> Self {
        Self {
            at,
            kind: TableRowBoundaryKind::Raw(raw.into()),
        }
    }
}

/// The raw, invisible XML represented by a [`TableRowBoundary`]. SDT opens and
/// closes affect row ownership; `Raw` preserves an otherwise unknown table
/// child at its exact row gap without affecting the control stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableRowBoundaryKind {
    /// Verbatim `<w:sdt>...<w:sdtContent>` prefix.
    SdtOpen(String),
    /// Verbatim `</w:sdtContent></w:sdt>` suffix.
    SdtClose(String),
    /// Any other invisible table child found between rows or SDT boundaries.
    Raw(String),
}

/// A violated [`Table::row_boundaries`] invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableRowBoundaryError {
    PositionOutOfBounds {
        boundary_index: usize,
        at: usize,
        row_count: usize,
    },
    PositionOutOfOrder {
        boundary_index: usize,
        previous_at: usize,
        at: usize,
    },
    UnexpectedClose {
        boundary_index: usize,
        at: usize,
    },
    UnclosedOpen {
        boundary_index: usize,
        at: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    /// Column widths in twips (`w:tblGrid`/`w:gridCol`).
    pub grid: Vec<u32>,
    pub rows: Vec<Row>,
    /// Namespace bindings inherited by preserved raw table children and not
    /// guaranteed at reconstructed ancestors. The serializer redeclares them
    /// on `w:tbl` because document and body ancestors are rebuilt during save.
    pub namespace_declarations: Vec<(String, String)>,
    /// Invisible row-level content-control boundaries and unknown table
    /// children, ordered by `at` and then by vector position.
    ///
    /// In a valid table every `at` is in `0..=rows.len()`, positions never
    /// decrease, and SDT opens/closes are balanced and properly nested. A row is
    /// owned by the SDTs left on the stack after all boundaries at its preceding
    /// gap have been applied. Consequently, nested and adjacent controls and an
    /// empty control (an open and close at the same gap) are all lossless while
    /// `rows` remains the ordinary visible/editable row list.
    ///
    /// Derived cloning copies boundary XML and derived equality compares it;
    /// plain text intentionally comes only from `rows`. New/default tables have
    /// no boundaries. Code changing row count should use [`Table::insert_row`]
    /// and [`Table::remove_row`] so anchors stay synchronized.
    pub row_boundaries: Vec<TableRowBoundary>,
    /// The table's entire `w:tblPr` verbatim (borders, shading, width, style,
    /// look, layout), preserved so save round-trips table formatting.
    pub raw_tblpr: Option<String>,
}

impl Table {
    /// Verify that row-boundary anchors are ordered, in range, and form a
    /// balanced, properly nested SDT stack.
    pub fn validate_row_boundaries(&self) -> Result<(), TableRowBoundaryError> {
        let mut previous_at = 0;
        let mut open_boundaries = Vec::new();

        for (boundary_index, boundary) in self.row_boundaries.iter().enumerate() {
            if boundary.at > self.rows.len() {
                return Err(TableRowBoundaryError::PositionOutOfBounds {
                    boundary_index,
                    at: boundary.at,
                    row_count: self.rows.len(),
                });
            }
            if boundary_index > 0 && boundary.at < previous_at {
                return Err(TableRowBoundaryError::PositionOutOfOrder {
                    boundary_index,
                    previous_at,
                    at: boundary.at,
                });
            }
            previous_at = boundary.at;

            match &boundary.kind {
                TableRowBoundaryKind::SdtOpen(_) => open_boundaries.push(boundary_index),
                TableRowBoundaryKind::SdtClose(_) => {
                    if open_boundaries.pop().is_none() {
                        return Err(TableRowBoundaryError::UnexpectedClose {
                            boundary_index,
                            at: boundary.at,
                        });
                    }
                }
                TableRowBoundaryKind::Raw(_) => {}
            }
        }

        if let Some(boundary_index) = open_boundaries.pop() {
            return Err(TableRowBoundaryError::UnclosedOpen {
                boundary_index,
                at: self.row_boundaries[boundary_index].at,
            });
        }
        Ok(())
    }

    /// Return the opening-boundary indices that own each visible row, ordered
    /// outermost to innermost. This is also the precise ownership rule used for
    /// edits at a control edge.
    pub fn row_control_owners(&self) -> Result<Vec<Vec<usize>>, TableRowBoundaryError> {
        self.validate_row_boundaries()?;
        let mut owners = Vec::with_capacity(self.rows.len());
        let mut open_boundaries = Vec::new();
        let mut boundary_index = 0;

        for row_index in 0..self.rows.len() {
            while boundary_index < self.row_boundaries.len()
                && self.row_boundaries[boundary_index].at == row_index
            {
                match &self.row_boundaries[boundary_index].kind {
                    TableRowBoundaryKind::SdtOpen(_) => open_boundaries.push(boundary_index),
                    TableRowBoundaryKind::SdtClose(_) => {
                        open_boundaries.pop();
                    }
                    TableRowBoundaryKind::Raw(_) => {}
                }
                boundary_index += 1;
            }
            owners.push(open_boundaries.clone());
        }
        Ok(owners)
    }

    /// Insert before `index`, keeping every boundary at that gap before the new
    /// row. Therefore insertion at a control's first row inherits that row's
    /// ownership, while insertion immediately after its last row (after the
    /// close boundary) stays outside. Returns `false` for an invalid index.
    pub fn insert_row(&mut self, index: usize, row: Row) -> bool {
        if index > self.rows.len() {
            return false;
        }
        for boundary in &mut self.row_boundaries {
            if boundary.at > index {
                boundary.at += 1;
            }
        }
        self.rows.insert(index, row);
        true
    }

    /// Remove a visible row and collapse the following row gap onto its
    /// preceding gap. Deleting a control's first or last row leaves every
    /// remaining group row under the same boundaries; deleting its final visible
    /// row retains a balanced empty control rather than discarding its definition.
    pub fn remove_row(&mut self, index: usize) -> Option<Row> {
        if index >= self.rows.len() {
            return None;
        }
        let row = self.rows.remove(index);
        for boundary in &mut self.row_boundaries {
            if boundary.at > index {
                boundary.at -= 1;
            }
        }
        Some(row)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        Row {
            cells: vec![Cell {
                blocks: vec![Block::Paragraph(Paragraph {
                    content: vec![Inline::Run(Run {
                        text: text.to_string(),
                        ..Default::default()
                    })],
                    ..Default::default()
                })],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn table(texts: &[&str], row_boundaries: Vec<TableRowBoundary>) -> Table {
        Table {
            rows: texts.iter().map(|text| row(text)).collect(),
            row_boundaries,
            ..Default::default()
        }
    }

    fn open(at: usize, name: &str) -> TableRowBoundary {
        TableRowBoundary::sdt_open(
            at,
            format!("<w:sdt><w:sdtPr><w:alias w:val=\"{name}\"/></w:sdtPr><w:sdtContent>"),
        )
    }

    fn close(at: usize) -> TableRowBoundary {
        TableRowBoundary::sdt_close(at, "</w:sdtContent></w:sdt>")
    }

    #[test]
    fn plain_rows_and_default_tables_have_no_boundary_owners() {
        let empty = Table::default();
        assert!(empty.row_boundaries.is_empty());
        assert_eq!(empty.row_control_owners(), Ok(Vec::new()));

        let plain = table(&["one", "two"], vec![]);
        assert_eq!(plain.row_control_owners(), Ok(vec![vec![], vec![]]));
        assert_eq!(Block::Table(plain).plain_text(), "one\ntwo\n");
    }

    #[test]
    fn one_controlled_row_is_visible_cloneable_and_compared_with_its_wrapper() {
        let controlled = table(&["visible"], vec![open(0, "single"), close(1)]);
        assert_eq!(controlled.row_control_owners(), Ok(vec![vec![0]]));
        assert_eq!(Block::Table(controlled.clone()).plain_text(), "visible\n");
        assert_eq!(controlled.clone(), controlled);

        let mut changed_wrapper = controlled.clone();
        changed_wrapper.row_boundaries[0] = open(0, "changed");
        assert_ne!(changed_wrapper, controlled);
    }

    #[test]
    fn multiple_controlled_rows_follow_edge_insert_and_delete_rules() {
        let mut controlled = table(&["first", "last"], vec![open(0, "group"), close(2)]);

        assert!(controlled.insert_row(0, row("inserted at first")));
        assert_eq!(controlled.row_boundaries[1].at, 3);
        assert_eq!(
            controlled.row_control_owners(),
            Ok(vec![vec![0], vec![0], vec![0]])
        );

        assert!(controlled.insert_row(3, row("inserted after last")));
        assert_eq!(
            controlled.row_control_owners(),
            Ok(vec![vec![0], vec![0], vec![0], vec![]])
        );

        assert_eq!(controlled.remove_row(0), Some(row("inserted at first")));
        assert_eq!(controlled.remove_row(0), Some(row("first")));
        assert_eq!(controlled.remove_row(0), Some(row("last")));
        assert_eq!(controlled.row_boundaries[0].at, 0);
        assert_eq!(controlled.row_boundaries[1].at, 0);
        assert_eq!(controlled.row_control_owners(), Ok(vec![vec![]]));
        assert!(controlled.validate_row_boundaries().is_ok());
    }

    #[test]
    fn inserted_rows_follow_nested_and_adjacent_edge_ownership() {
        let mut nested = table(
            &["outer", "inner", "outer again"],
            vec![open(0, "outer"), open(1, "inner"), close(2), close(3)],
        );
        assert!(nested.insert_row(1, row("inserted at inner first row")));
        assert_eq!(
            nested.row_control_owners(),
            Ok(vec![vec![0], vec![0, 1], vec![0, 1], vec![0]])
        );

        let mut adjacent = table(
            &["left", "right"],
            vec![open(0, "left"), close(1), open(1, "right"), close(2)],
        );
        assert!(adjacent.insert_row(1, row("inserted at shared edge")));
        assert_eq!(
            adjacent.row_control_owners(),
            Ok(vec![vec![0], vec![2], vec![2]])
        );
    }

    #[test]
    fn removed_rows_preserve_nested_and_adjacent_edge_ownership() {
        let mut nested = table(
            &["outer", "inner first", "inner last", "outer again"],
            vec![open(0, "outer"), open(1, "inner"), close(3), close(4)],
        );
        assert_eq!(nested.remove_row(1), Some(row("inner first")));
        assert_eq!(nested.remove_row(1), Some(row("inner last")));
        assert_eq!(nested.row_control_owners(), Ok(vec![vec![0], vec![0]]));
        assert_eq!(nested.row_boundaries[1].at, 1);
        assert_eq!(nested.row_boundaries[2].at, 1);
        assert!(nested.validate_row_boundaries().is_ok());

        let mut adjacent = table(
            &["left", "right"],
            vec![open(0, "left"), close(1), open(1, "right"), close(2)],
        );
        assert_eq!(adjacent.remove_row(0), Some(row("left")));
        assert_eq!(adjacent.row_control_owners(), Ok(vec![vec![2]]));
        assert_eq!(
            adjacent.row_boundaries,
            vec![open(0, "left"), close(0), open(0, "right"), close(1)]
        );
        assert!(adjacent.validate_row_boundaries().is_ok());
    }

    #[test]
    fn deleting_every_visible_row_retains_nonempty_control_definition() {
        let raw_child =
            TableRowBoundary::raw(1, "<w:customXml w:uri=\"urn:definition-metadata\"/>");
        let mut controlled = table(
            &["first", "last"],
            vec![open(0, "definition"), raw_child.clone(), close(2)],
        );

        assert_eq!(controlled.remove_row(0), Some(row("first")));
        assert_eq!(controlled.remove_row(0), Some(row("last")));
        assert!(controlled.rows.is_empty());
        assert_eq!(
            controlled.row_boundaries,
            vec![
                open(0, "definition"),
                TableRowBoundary { at: 0, ..raw_child },
                close(0),
            ]
        );
        assert!(controlled.validate_row_boundaries().is_ok());
    }

    #[test]
    fn nested_controls_record_outer_to_inner_row_ownership() {
        let nested = table(
            &["outer", "nested", "outer again"],
            vec![open(0, "outer"), open(1, "inner"), close(2), close(3)],
        );

        assert_eq!(
            nested.row_control_owners(),
            Ok(vec![vec![0], vec![0, 1], vec![0]])
        );
    }

    #[test]
    fn adjacent_controls_share_a_gap_without_sharing_rows() {
        let adjacent = table(
            &["left", "right"],
            vec![open(0, "left"), close(1), open(1, "right"), close(2)],
        );

        assert_eq!(adjacent.row_control_owners(), Ok(vec![vec![0], vec![2]]));
    }

    #[test]
    fn empty_control_and_unknown_child_are_preserved_at_one_gap() {
        let empty = table(
            &[],
            vec![
                open(0, "empty"),
                TableRowBoundary::raw(0, "<w:customXml w:uri=\"urn:test\"/>"),
                close(0),
            ],
        );

        assert!(empty.validate_row_boundaries().is_ok());
        assert_eq!(empty.row_control_owners(), Ok(Vec::new()));
        assert_eq!(Block::Table(empty.clone()).plain_text(), "");
        assert_eq!(empty.clone(), empty);
    }

    #[test]
    fn invalid_boundaries_report_range_order_and_balance_errors() {
        let out_of_bounds = table(&[], vec![open(1, "bad"), close(1)]);
        assert!(matches!(
            out_of_bounds.validate_row_boundaries(),
            Err(TableRowBoundaryError::PositionOutOfBounds { .. })
        ));

        let out_of_order = table(&["row"], vec![open(1, "bad"), close(0)]);
        assert!(matches!(
            out_of_order.validate_row_boundaries(),
            Err(TableRowBoundaryError::PositionOutOfOrder { .. })
        ));

        let unexpected_close = table(&[], vec![close(0)]);
        assert!(matches!(
            unexpected_close.validate_row_boundaries(),
            Err(TableRowBoundaryError::UnexpectedClose { .. })
        ));

        let unclosed = table(&[], vec![open(0, "bad")]);
        assert!(matches!(
            unclosed.validate_row_boundaries(),
            Err(TableRowBoundaryError::UnclosedOpen { .. })
        ));
    }
}
