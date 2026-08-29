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
    /// `w:spacing`/`w:kern`, `w:lang`, `w:shd`, `w:effect`, …), plus explicit-off
    /// toggles that must remain distinct from an absent/style-derived value.
    /// Preserved children are re-emitted in schema order.
    pub raw_props: Vec<String>,
    /// A tracked `w:rPrChange`, when present. The owning `RunProps` is the
    /// current state; `previous` on the change retains the prior snapshot.
    pub property_change: Option<PropertyChange>,
    /// Display-only formatting contributed by enclosing revision wrappers.
    ///
    /// The public formatting fields remain the effective render state. These
    /// counters remember whether underline/strike was added solely for review
    /// display so accepting or rejecting a wrapper never removes genuine direct
    /// formatting. They are not serialized as document properties.
    #[doc(hidden)]
    pub revision_cues: RevisionDisplayCues,
}

/// Provenance for the underline/strike cues used to render tracked changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct RevisionDisplayCues {
    pub insertions: u32,
    pub deletions: u32,
    pub underline_added: bool,
    pub strike_added: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub props: RunProps,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hyperlink {
    /// Resolved target URL (external link) if present.
    pub target: Option<String>,
    /// In-document anchor (`w:anchor`) if present.
    pub anchor: Option<String>,
    /// Original relationship id (`r:id`), preserved so save can write it back
    /// unchanged (the `.rels` part itself is preserved verbatim).
    pub rel_id: Option<String>,
    pub runs: Vec<Run>,
    /// Full child sequence for hyperlinks that contain revisions or other
    /// non-run markup. Simple editable links continue to use `runs`.
    pub content: Vec<Inline>,
    /// Original complete hyperlink XML for byte-faithful untouched saves.
    pub raw: Option<String>,
    /// Set when a review action changes a descendant of `content`.
    #[doc(hidden)]
    pub content_changed: bool,
}

impl Hyperlink {
    /// Visible runs in source order, including runs nested in revision wrappers.
    pub fn visible_runs(&self) -> Vec<&Run> {
        fn collect<'a>(content: &'a [Inline], out: &mut Vec<&'a Run>) {
            for inline in content {
                match inline {
                    Inline::Run(run) => out.push(run),
                    Inline::Hyperlink(link) => {
                        out.extend(link.runs.iter());
                        collect(&link.content, out);
                    }
                    Inline::Revision { content, .. } => collect(content, out),
                    _ => {}
                }
            }
        }

        let mut runs = self.runs.iter().collect::<Vec<_>>();
        collect(&self.content, &mut runs);
        runs
    }
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
        metadata: RevisionMetadata,
        raw: String,
        content: Vec<Inline>,
        /// Set after an action changes a descendant. Untouched wrappers continue
        /// to serialize from `raw`; changed wrappers rebuild only their content.
        #[doc(hidden)]
        content_changed: bool,
    },
    /// A recognized but deliberately unsupported revision record (move ranges,
    /// custom-XML ranges, and table-cell revision records). Keeping it distinct
    /// from generic raw XML lets review commands report it instead of silently
    /// treating it as ordinary content.
    UnsupportedRevision {
        kind: UnsupportedRevisionKind,
        metadata: RevisionMetadata,
        raw: String,
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

/// Stable, document-local identity for a revision node.
///
/// Zero means "not assigned yet". Loaders call
/// [`Document::initialize_revision_targets`] after building the tree. The value
/// then lives on the node, so removing or unwrapping an earlier revision does
/// not invalidate targets held by the editor or automation layer. Cloning a
/// document preserves these identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RevisionTarget(pub u64);

impl RevisionTarget {
    pub fn is_assigned(self) -> bool {
        self.0 != 0
    }
}

/// Common `CT_TrackChange` metadata. Values remain strings because producer
/// files can contain non-numeric ids and non-normalized dates. Unknown
/// attributes are decoded for inspection while the owning node's `raw` XML is
/// retained for byte-faithful serialization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RevisionMetadata {
    pub target: RevisionTarget,
    pub id: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    pub unknown_attributes: Vec<(String, String)>,
}

/// Property containers for which WordprocessingML defines a `*PrChange` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PropertyScope {
    #[default]
    Run,
    Paragraph,
    Table,
    TableRow,
    TableCell,
    Section,
}

/// The parsed prior-property payload captured from a `*PrChange` record.
///
/// Run and paragraph properties use the same semantic types as their current
/// owners. Table, row, cell, and section properties are not otherwise expanded
/// by the editable model, so their scoped property container remains exact XML.
/// In every case [`PropertyChange::raw`] retains the complete change wrapper,
/// including producer-specific metadata and children.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PropertySnapshot {
    /// A present, structurally valid prior property container.
    Present(PropertyState),
    /// The change record had no prior property container.
    #[default]
    Absent,
    /// Source intended to carry a snapshot but was structurally malformed.
    Malformed(String),
}

/// Scope-typed state held by [`PropertySnapshot::Present`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyState {
    Run(Box<RunProps>),
    Paragraph(Box<ParProps>),
    Table(String),
    TableRow(String),
    TableCell(String),
    Section(String),
}

impl PropertyState {
    pub fn scope(&self) -> PropertyScope {
        match self {
            PropertyState::Run(_) => PropertyScope::Run,
            PropertyState::Paragraph(_) => PropertyScope::Paragraph,
            PropertyState::Table(_) => PropertyScope::Table,
            PropertyState::TableRow(_) => PropertyScope::TableRow,
            PropertyState::TableCell(_) => PropertyScope::TableCell,
            PropertyState::Section(_) => PropertyScope::Section,
        }
    }

    /// Exact XML for scopes whose current state is also stored as raw XML.
    pub fn raw_xml(&self) -> Option<&str> {
        match self {
            PropertyState::Table(raw)
            | PropertyState::TableRow(raw)
            | PropertyState::TableCell(raw)
            | PropertyState::Section(raw) => Some(raw),
            PropertyState::Run(_) | PropertyState::Paragraph(_) => None,
        }
    }
}

/// A tracked property change attached to its current property owner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropertyChange {
    pub scope: PropertyScope,
    pub metadata: RevisionMetadata,
    /// Verbatim `*PrChange` element, including unknown attributes/children.
    pub raw: String,
    pub previous: PropertySnapshot,
}

/// Recognized revision markup that is outside the supported accept/reject set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedRevisionKind {
    MoveFrom,
    MoveTo,
    MoveFromRangeStart,
    MoveFromRangeEnd,
    MoveToRangeStart,
    MoveToRangeEnd,
    CustomXmlInsRangeStart,
    CustomXmlInsRangeEnd,
    CustomXmlDelRangeStart,
    CustomXmlDelRangeEnd,
    CustomXmlMoveFromRangeStart,
    CustomXmlMoveFromRangeEnd,
    CustomXmlMoveToRangeStart,
    CustomXmlMoveToRangeEnd,
    CellInsert,
    CellDelete,
    CellMerge,
    ConflictInsert,
    ConflictDelete,
    /// Future or producer-specific revision markup classified by a caller.
    Other(String),
}

/// A recognized unsupported record retained inside a property container whose
/// raw XML remains authoritative for serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPropertyRevision {
    pub kind: UnsupportedRevisionKind,
    pub metadata: RevisionMetadata,
}

/// Reviewable and reportable revision categories in one document-order list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionCategory {
    Inline(RevisionKind),
    Property(PropertyScope),
    Unsupported(UnsupportedRevisionKind),
}

/// A current document-order view of a stable revision target.
///
/// `ordinal` is recalculated on every enumeration and is display-only. Actions
/// resolve `target`, never a cached ordinal or a flat block index. `parent`
/// records wrapper nesting; outer revisions precede their descendants in source
/// order, while transforms can use `depth` to process innermost targets first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionAddress {
    pub target: RevisionTarget,
    pub ordinal: usize,
    pub parent: Option<RevisionTarget>,
    pub depth: usize,
    pub category: RevisionCategory,
    pub metadata: RevisionMetadata,
}

impl Inline {
    /// The visible text this inline contributes (tabs/breaks as whitespace).
    pub fn text(&self) -> String {
        match self {
            Inline::Run(r) => r.text.clone(),
            Inline::Hyperlink(h) if !h.content.is_empty() => {
                h.content.iter().map(Inline::text).collect()
            }
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
            Inline::UnsupportedRevision { .. } => String::new(),
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
    /// `w:keepNext`, `w:outlineLvl`, …), plus explicit default/off values such as
    /// direct-left `w:jc` and disabled `w:bidi`. Preserved so save does not
    /// confuse a direct override with style inheritance.
    pub raw_props: Vec<String>,
    /// A tracked `w:pPrChange`; the remaining fields are the current state.
    pub property_change: Option<PropertyChange>,
    /// A `w:sectPrChange` nested in this paragraph's section-break properties.
    pub section_property_change: Option<PropertyChange>,
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
    pub property_change: Option<PropertyChange>,
    /// Recognized `w:cellIns`/`w:cellDel`/`w:cellMerge` records retained in
    /// `raw_tcpr`, but modeled separately so review can enumerate and reject
    /// actions against them explicitly.
    pub unsupported_revisions: Vec<UnsupportedPropertyRevision>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: Vec::new(),
            raw_tcpr: None,
            property_change: None,
            unsupported_revisions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Verbatim `w:trPr` / `w:tblPrEx` XML (row height, header flag, exceptions)
    /// plus row-level content-control boundary XML. Properties are re-emitted
    /// inside the row; recognized boundaries are re-emitted immediately outside
    /// the first/last wrapped row.
    pub raw_props: Vec<String>,
    pub property_change: Option<PropertyChange>,
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
    /// Effective markup-compatibility attributes inherited from reconstructed
    /// ancestors such as `w:document`/`w:body`, or declared on `w:tbl` itself.
    /// Values with the same expanded name are token-unioned by the loader and
    /// redeclared on `w:tbl` so preserved extension markup keeps its MCE semantics.
    pub markup_compatibility_attributes: Vec<(String, String)>,
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
    pub property_change: Option<PropertyChange>,
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

/// Body-level trailing section properties. Unlike paragraph-nested section
/// breaks, this node describes the document's final section.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionProperties {
    /// Current `w:sectPr` container with its `w:sectPrChange` separated.
    pub raw: String,
    pub property_change: Option<PropertyChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    SectionProperties(SectionProperties),
    /// Verbatim XML for block-level content we don't model (content controls,
    /// etc.), preserved for lossless save.
    Raw(String),
}

impl Block {
    pub fn plain_text(&self) -> String {
        match self {
            Block::Raw(_) | Block::SectionProperties(_) => String::new(),
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
    /// Number of top-level content blocks exposed through editor and agent
    /// block coordinates. A body-level `w:sectPr` is modeled as a trailing
    /// sentinel so its revision can be reviewed, but it is not a visible or
    /// editable document block.
    pub fn content_block_count(&self) -> usize {
        self.body
            .iter()
            .position(|block| matches!(block, Block::SectionProperties(_)))
            .unwrap_or(self.body.len())
    }

    /// The body-level properties for the document's final section, when the
    /// source document supplied an explicit `w:sectPr`.
    pub fn trailing_section_properties(&self) -> Option<&SectionProperties> {
        self.body.iter().rev().find_map(|block| match block {
            Block::SectionProperties(section) => Some(section),
            _ => None,
        })
    }

    pub fn trailing_section_properties_mut(&mut self) -> Option<&mut SectionProperties> {
        self.body.iter_mut().rev().find_map(|block| match block {
            Block::SectionProperties(section) => Some(section),
            _ => None,
        })
    }

    /// Concatenated visible text, one block per line — handy for tests/sanity.
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for b in &self.body {
            s.push_str(&b.plain_text());
            if !matches!(b, Block::Table(_) | Block::SectionProperties(_)) {
                s.push('\n');
            }
        }
        s
    }

    /// Assign identities to newly parsed or manually-created revision nodes.
    /// Existing nonzero identities are never renumbered, which is the stability
    /// guarantee needed when an earlier action changes document layout.
    pub fn initialize_revision_targets(&mut self) {
        let mut next = self
            .revisions()
            .into_iter()
            .map(|address| address.target.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        let mut seen = std::collections::HashSet::new();
        for block in &mut self.body {
            assign_block_revision_targets(block, &mut next, &mut seen);
        }
    }

    /// Enumerate modeled and recognized-unsupported revisions in current source
    /// order. Addresses contain stable node identities, not structural paths.
    pub fn revisions(&self) -> Vec<RevisionAddress> {
        let mut out = Vec::new();
        for block in &self.body {
            collect_block_revisions(block, None, 0, &mut out);
        }
        out
    }

    /// Resolve a stable target against the current tree after intervening edits.
    pub fn revision(&self, target: RevisionTarget) -> Option<RevisionAddress> {
        self.revisions()
            .into_iter()
            .find(|address| address.target == target)
    }
}

fn assign_metadata_target(
    metadata: &mut RevisionMetadata,
    next: &mut u64,
    seen: &mut std::collections::HashSet<RevisionTarget>,
) {
    if !metadata.target.is_assigned() || !seen.insert(metadata.target) {
        metadata.target = RevisionTarget(*next);
        *next = next.saturating_add(1);
        seen.insert(metadata.target);
    }
}

fn assign_property_target(
    change: &mut Option<PropertyChange>,
    next: &mut u64,
    seen: &mut std::collections::HashSet<RevisionTarget>,
) {
    if let Some(change) = change {
        assign_metadata_target(&mut change.metadata, next, seen);
    }
}

fn assign_run_props_target(
    props: &mut RunProps,
    next: &mut u64,
    seen: &mut std::collections::HashSet<RevisionTarget>,
) {
    assign_property_target(&mut props.property_change, next, seen);
}

fn assign_inline_revision_targets(
    inline: &mut Inline,
    next: &mut u64,
    seen: &mut std::collections::HashSet<RevisionTarget>,
) {
    match inline {
        Inline::Run(run) => assign_run_props_target(&mut run.props, next, seen),
        Inline::Hyperlink(link) => {
            for run in &mut link.runs {
                assign_run_props_target(&mut run.props, next, seen);
            }
            for child in &mut link.content {
                assign_inline_revision_targets(child, next, seen);
            }
        }
        Inline::Tab(props) => assign_run_props_target(props, next, seen),
        Inline::TextBox { blocks, .. } => {
            for block in blocks {
                assign_block_revision_targets(block, next, seen);
            }
        }
        Inline::Revision {
            metadata, content, ..
        } => {
            assign_metadata_target(metadata, next, seen);
            for child in content {
                assign_inline_revision_targets(child, next, seen);
            }
        }
        Inline::UnsupportedRevision { metadata, .. } => {
            assign_metadata_target(metadata, next, seen)
        }
        Inline::Break(_)
        | Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => {}
    }
}

fn assign_block_revision_targets(
    block: &mut Block,
    next: &mut u64,
    seen: &mut std::collections::HashSet<RevisionTarget>,
) {
    match block {
        Block::Paragraph(paragraph) => {
            // sectPr precedes pPrChange in CT_PPr schema order.
            assign_property_target(&mut paragraph.props.section_property_change, next, seen);
            assign_property_target(&mut paragraph.props.property_change, next, seen);
            for inline in &mut paragraph.content {
                assign_inline_revision_targets(inline, next, seen);
            }
        }
        Block::Table(table) => {
            assign_property_target(&mut table.property_change, next, seen);
            for row in &mut table.rows {
                assign_property_target(&mut row.property_change, next, seen);
                for cell in &mut row.cells {
                    assign_property_target(&mut cell.property_change, next, seen);
                    for revision in &mut cell.unsupported_revisions {
                        assign_metadata_target(&mut revision.metadata, next, seen);
                    }
                    for block in &mut cell.blocks {
                        assign_block_revision_targets(block, next, seen);
                    }
                }
            }
        }
        Block::SectionProperties(section) => {
            assign_property_target(&mut section.property_change, next, seen);
        }
        Block::Raw(_) => {}
    }
}

fn push_revision_address(
    metadata: &RevisionMetadata,
    category: RevisionCategory,
    parent: Option<RevisionTarget>,
    depth: usize,
    out: &mut Vec<RevisionAddress>,
) {
    out.push(RevisionAddress {
        target: metadata.target,
        ordinal: out.len(),
        parent,
        depth,
        category,
        metadata: metadata.clone(),
    });
}

fn collect_property_revision(
    change: &Option<PropertyChange>,
    parent: Option<RevisionTarget>,
    depth: usize,
    out: &mut Vec<RevisionAddress>,
) {
    if let Some(change) = change {
        push_revision_address(
            &change.metadata,
            RevisionCategory::Property(change.scope),
            parent,
            depth,
            out,
        );
    }
}

fn collect_run_props_revisions(
    props: &RunProps,
    parent: Option<RevisionTarget>,
    depth: usize,
    out: &mut Vec<RevisionAddress>,
) {
    collect_property_revision(&props.property_change, parent, depth, out);
}

fn collect_inline_revisions(
    inline: &Inline,
    parent: Option<RevisionTarget>,
    depth: usize,
    out: &mut Vec<RevisionAddress>,
) {
    match inline {
        Inline::Run(run) => collect_run_props_revisions(&run.props, parent, depth, out),
        Inline::Hyperlink(link) => {
            for run in &link.runs {
                collect_run_props_revisions(&run.props, parent, depth, out);
            }
            for child in &link.content {
                collect_inline_revisions(child, parent, depth, out);
            }
        }
        Inline::Tab(props) => collect_run_props_revisions(props, parent, depth, out),
        Inline::TextBox { blocks, .. } => {
            for block in blocks {
                collect_block_revisions(block, parent, depth, out);
            }
        }
        Inline::Revision {
            kind,
            metadata,
            content,
            ..
        } => {
            push_revision_address(
                metadata,
                RevisionCategory::Inline(*kind),
                parent,
                depth,
                out,
            );
            let child_parent = Some(metadata.target);
            for child in content {
                collect_inline_revisions(child, child_parent, depth + 1, out);
            }
        }
        Inline::UnsupportedRevision { kind, metadata, .. } => push_revision_address(
            metadata,
            RevisionCategory::Unsupported(kind.clone()),
            parent,
            depth,
            out,
        ),
        Inline::Break(_)
        | Inline::SmartArt { .. }
        | Inline::Chart { .. }
        | Inline::Equation { .. }
        | Inline::Field { .. }
        | Inline::FootnoteRef { .. }
        | Inline::Raw(_) => {}
    }
}

fn collect_block_revisions(
    block: &Block,
    parent: Option<RevisionTarget>,
    depth: usize,
    out: &mut Vec<RevisionAddress>,
) {
    match block {
        Block::Paragraph(paragraph) => {
            collect_property_revision(&paragraph.props.section_property_change, parent, depth, out);
            collect_property_revision(&paragraph.props.property_change, parent, depth, out);
            for inline in &paragraph.content {
                collect_inline_revisions(inline, parent, depth, out);
            }
        }
        Block::Table(table) => {
            collect_property_revision(&table.property_change, parent, depth, out);
            for row in &table.rows {
                collect_property_revision(&row.property_change, parent, depth, out);
                for cell in &row.cells {
                    collect_property_revision(&cell.property_change, parent, depth, out);
                    for revision in &cell.unsupported_revisions {
                        push_revision_address(
                            &revision.metadata,
                            RevisionCategory::Unsupported(revision.kind.clone()),
                            parent,
                            depth,
                            out,
                        );
                    }
                    for block in &cell.blocks {
                        collect_block_revisions(block, parent, depth, out);
                    }
                }
            }
        }
        Block::SectionProperties(section) => {
            collect_property_revision(&section.property_change, parent, depth, out);
        }
        Block::Raw(_) => {}
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

    fn revision_inline(kind: RevisionKind, id: Option<&str>, content: Vec<Inline>) -> Inline {
        Inline::Revision {
            kind,
            metadata: RevisionMetadata {
                id: id.map(str::to_string),
                ..RevisionMetadata::default()
            },
            raw: format!("<w:{:?}/>", kind),
            content,
            content_changed: false,
        }
    }

    fn property_change(scope: PropertyScope, prior: &str) -> Option<PropertyChange> {
        let state = match scope {
            PropertyScope::Run => PropertyState::Run(Box::default()),
            PropertyScope::Paragraph => PropertyState::Paragraph(Box::default()),
            PropertyScope::Table => PropertyState::Table(prior.to_string()),
            PropertyScope::TableRow => PropertyState::TableRow(prior.to_string()),
            PropertyScope::TableCell => PropertyState::TableCell(prior.to_string()),
            PropertyScope::Section => PropertyState::Section(prior.to_string()),
        };
        Some(PropertyChange {
            scope,
            metadata: RevisionMetadata::default(),
            raw: format!("<{scope:?}Change>{prior}</{scope:?}Change>"),
            previous: PropertySnapshot::Present(state),
        })
    }

    #[test]
    fn inline_revision_metadata_nesting_and_clone_are_structural() {
        let inner = revision_inline(
            RevisionKind::Delete,
            None,
            vec![Inline::Run(Run {
                text: "old".to_string(),
                ..Run::default()
            })],
        );
        let mut outer_metadata = RevisionMetadata {
            id: Some("7".to_string()),
            author: Some("Ada".to_string()),
            date: Some("2026-08-29T10:30:00Z".to_string()),
            unknown_attributes: vec![("w16du:dateUtc".to_string(), "raw-date".to_string())],
            ..RevisionMetadata::default()
        };
        // Targets are deliberately absent until the complete document tree can
        // assign them in deterministic source order.
        assert!(!outer_metadata.target.is_assigned());
        let outer = Inline::Revision {
            kind: RevisionKind::Insert,
            metadata: std::mem::take(&mut outer_metadata),
            raw: "<w:ins w:id=\"7\" w:future=\"kept\">...</w:ins>".to_string(),
            content: vec![inner],
            content_changed: false,
        };
        let mut document = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![outer],
                ..Paragraph::default()
            })],
        };

        document.initialize_revision_targets();
        let revisions = document.revisions();
        assert_eq!(revisions.len(), 2);
        assert_eq!(
            revisions[0].category,
            RevisionCategory::Inline(RevisionKind::Insert)
        );
        assert_eq!(revisions[0].parent, None);
        assert_eq!(revisions[0].depth, 0);
        assert_eq!(revisions[0].metadata.author.as_deref(), Some("Ada"));
        assert_eq!(
            revisions[0].metadata.date.as_deref(),
            Some("2026-08-29T10:30:00Z")
        );
        assert_eq!(
            revisions[0].metadata.unknown_attributes[0].0,
            "w16du:dateUtc"
        );
        assert_eq!(
            revisions[1].category,
            RevisionCategory::Inline(RevisionKind::Delete)
        );
        assert_eq!(revisions[1].parent, Some(revisions[0].target));
        assert_eq!(revisions[1].depth, 1);
        assert!(revisions[1].metadata.id.is_none());
        assert_eq!(document.clone(), document, "clone changed revision data");
    }

    #[test]
    fn every_property_scope_has_current_owner_and_prior_snapshot() {
        let run = Inline::Run(Run {
            text: "cell".to_string(),
            props: RunProps {
                property_change: property_change(PropertyScope::Run, "<w:rPr><w:b/></w:rPr>"),
                ..RunProps::default()
            },
        });
        let paragraph = Block::Paragraph(Paragraph {
            props: ParProps {
                property_change: property_change(
                    PropertyScope::Paragraph,
                    "<w:pPr><w:jc w:val=\"left\"/></w:pPr>",
                ),
                section_property_change: property_change(
                    PropertyScope::Section,
                    "<w:sectPr><w:pgSz w:w=\"12240\"/></w:sectPr>",
                ),
                ..ParProps::default()
            },
            content: vec![run],
        });
        let mut document = Document {
            body: vec![Block::Table(Table {
                property_change: property_change(
                    PropertyScope::Table,
                    "<w:tblPr><w:tblStyle w:val=\"Old\"/></w:tblPr>",
                ),
                rows: vec![Row {
                    property_change: property_change(
                        PropertyScope::TableRow,
                        "<w:trPr><w:tblHeader/></w:trPr>",
                    ),
                    cells: vec![Cell {
                        property_change: property_change(
                            PropertyScope::TableCell,
                            "<w:tcPr><w:shd w:fill=\"FFFF00\"/></w:tcPr>",
                        ),
                        blocks: vec![paragraph],
                        ..Cell::default()
                    }],
                    ..Row::default()
                }],
                ..Table::default()
            })],
        };

        document.initialize_revision_targets();
        let revisions = document.revisions();
        let scopes = revisions
            .iter()
            .map(|revision| match revision.category {
                RevisionCategory::Property(scope) => scope,
                _ => panic!("unexpected non-property revision"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            scopes,
            vec![
                PropertyScope::Table,
                PropertyScope::TableRow,
                PropertyScope::TableCell,
                PropertyScope::Section,
                PropertyScope::Paragraph,
                PropertyScope::Run,
            ]
        );
        assert!(
            revisions
                .iter()
                .all(|revision| revision.target.is_assigned())
        );
        let cloned = document.clone();
        assert_eq!(cloned.revisions(), revisions);
        assert_eq!(cloned, document);
    }

    #[test]
    fn stable_target_resolves_after_an_earlier_revision_disappears() {
        let mut document = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![
                    revision_inline(RevisionKind::Insert, Some("1"), Vec::new()),
                    revision_inline(RevisionKind::Delete, Some("2"), Vec::new()),
                ],
                ..Paragraph::default()
            })],
        };
        document.initialize_revision_targets();
        let target = document.revisions()[1].target;

        let Block::Paragraph(paragraph) = &mut document.body[0] else {
            unreachable!()
        };
        paragraph.content.remove(0);

        let resolved = document
            .revision(target)
            .expect("stable target survives reindex");
        assert_eq!(resolved.ordinal, 0);
        assert_eq!(resolved.metadata.id.as_deref(), Some("2"));
    }

    #[test]
    fn missing_metadata_and_unknown_revision_kind_remain_reportable() {
        let raw = "<w:futureRevision data=\"opaque\"><w:payload/></w:futureRevision>";
        let mut document = Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![Inline::UnsupportedRevision {
                    kind: UnsupportedRevisionKind::Other("w:futureRevision".to_string()),
                    metadata: RevisionMetadata::default(),
                    raw: raw.to_string(),
                }],
                ..Paragraph::default()
            })],
        };
        document.initialize_revision_targets();

        let address = &document.revisions()[0];
        assert_eq!(
            address.category,
            RevisionCategory::Unsupported(UnsupportedRevisionKind::Other(
                "w:futureRevision".to_string()
            ))
        );
        assert!(address.metadata.id.is_none());
        assert!(address.metadata.author.is_none());
        let Block::Paragraph(paragraph) = &document.body[0] else {
            unreachable!()
        };
        let Inline::UnsupportedRevision {
            raw: cloned_raw, ..
        } = &paragraph.content[0]
        else {
            unreachable!()
        };
        assert_eq!(cloned_raw, raw);
        assert_eq!(document.clone(), document);
    }
}
