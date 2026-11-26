//! An HTML table laid out as a Word table (#1878): `colspan` / `rowspan` as
//! `gridSpan` / `vMerge` over a slot grid, column widths from the table's
//! and the cells' `width`, cell shading, vertical alignment and margins, and
//! the table's `border`, `cellpadding`, `cellspacing` and `align`.
//!
//! The HTML reader collects a [`RawTable`] per `<table>` (its cells' blocks
//! and the specs their elements computed); nested tables stay raw inside
//! their cell until the outermost table ends, so [`layout`] lays them out
//! top-down, each with its cell's width as the room it has.
//!
//! Units: widths are twips (a CSS px is 15), border sizes eighths of a
//! point (a px is 6).

use super::{Budget, MAX_COLUMNS, css};
use crate::model::{Block, Cell, ParBorder, Paragraph, Row, Table, VMerge};

/// The narrowest column, in twips (one px).
pub(crate) const MIN_COL: u32 = 15;
/// Word's widest page measure, in twips (22"): no width exceeds it.
pub(crate) const MAX_TWIPS: u32 = 31_680;
/// The page an HTML import is laid out for when the page declares none
/// (#1928): US Letter, written explicitly so a reader's locale (A4) never
/// changes the body width.
pub(crate) const PAGE_W_TWIPS: u32 = 12_240;
pub(crate) const PAGE_H_TWIPS: u32 = 15_840;
/// The page's margin on every side (#1928): 15 px, about the inset Outlook's
/// reading pane gives a mail body (a browser's own body margin is 8 px),
/// instead of Word's 1".
pub(crate) const PAGE_MARGIN_TWIPS: u32 = 225;
/// The room an outermost table has on that page: its body, 786 px.
pub(crate) const BODY_TWIPS: u32 = PAGE_W_TWIPS - 2 * PAGE_MARGIN_TWIPS;

/// The smallest page side `@page`'s `size` gives (#1944), 1"; the largest
/// is Word's [`MAX_TWIPS`].
const MIN_PAGE_TWIPS: u32 = 1_440;
/// The narrowest and shortest body `@page`'s margins may leave (#1944),
/// 0.5": margins leaving less are all ignored.
const MIN_BODY_TWIPS: u32 = 720;
/// The furthest a header or footer is from its page edge when the page
/// does not say (#1944): Word's 0.5", or the margin when that is less.
const HEADER_TWIPS: u32 = 720;

/// The page of an HTML import (#1928, #1944), in twips: its size, margins,
/// and header and footer distances. The reader lays outermost tables, wide
/// pictures and centred fixed-width boxes out in its [`Page::body`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Page {
    pub w: u32,
    pub h: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
    pub header: u32,
    pub footer: u32,
}

impl Page {
    /// #1928's page: Letter, 15 px on every side.
    pub(crate) const DEFAULT: Page = Page {
        w: PAGE_W_TWIPS,
        h: PAGE_H_TWIPS,
        top: PAGE_MARGIN_TWIPS,
        right: PAGE_MARGIN_TWIPS,
        bottom: PAGE_MARGIN_TWIPS,
        left: PAGE_MARGIN_TWIPS,
        header: PAGE_MARGIN_TWIPS,
        footer: PAGE_MARGIN_TWIPS,
    };

    /// The width between the side margins.
    pub(crate) fn body(&self) -> u32 {
        self.w.saturating_sub(self.left + self.right)
    }

    /// The page `@page`'s declarations give (#1944), in cascade order
    /// ([`css::StyleSheet::page_decls`]): [`Page::DEFAULT`] with each
    /// readable one applied, a later one winning.
    ///
    /// - `size`: one length (a square) or two, `auto`, a paper keyword
    ///   (`letter`, `legal`, `ledger`, `a3`-`a5`, `b4`, `b5`) and
    ///   `portrait` / `landscape`, either or both. A side outside 1"..22"
    ///   is ignored (the page keeps that side as it was).
    /// - `margin` and its longhands; `mso-header-margin` and
    ///   `mso-footer-margin` (Word's), else each is its side's margin, at
    ///   most 0.5" (ours: Word's filtered HTML drops them, and Word's own
    ///   default is 0.5"). A percentage, a negative or unreadable value is
    ///   ignored, as are all four margins when they would leave a body
    ///   narrower or shorter than 0.5".
    pub(crate) fn from_decls(decls: &[&css::Decl]) -> Page {
        let mut page = Page::DEFAULT;
        let mut margins = [None::<u32>; 4];
        let (mut header, mut footer) = (None, None);
        for d in decls {
            let v = d.value.as_str();
            match d.name.as_str() {
                "size" => {
                    if let Some((w, h)) = page_size(v) {
                        page.w = w.unwrap_or(page.w);
                        page.h = h.unwrap_or(page.h);
                    }
                }
                "margin" => {
                    if let Some(sides) = css::box_sides(v) {
                        for (m, side) in margins.iter_mut().zip(sides) {
                            if let Some(t) = side.and_then(page_twips) {
                                *m = Some(t);
                            }
                        }
                    }
                }
                "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => {
                    let i = match d.name.as_str() {
                        "margin-top" => 0,
                        "margin-right" => 1,
                        "margin-bottom" => 2,
                        _ => 3,
                    };
                    if let Some(t) = css::side(v).ok().flatten().and_then(page_twips) {
                        margins[i] = Some(t);
                    }
                }
                "mso-header-margin" | "mso-footer-margin" => {
                    let t = css::length(v)
                        .filter(|l| l.unit != css::Unit::Percent)
                        .and_then(page_twips);
                    if t.is_some() {
                        if d.name == "mso-header-margin" {
                            header = t;
                        } else {
                            footer = t;
                        }
                    }
                }
                _ => {}
            }
        }
        let [top, right, bottom, left] = margins.map(|m| m.unwrap_or(PAGE_MARGIN_TWIPS));
        let fits = |side: u32, a: u32, b: u32| {
            u64::from(side) >= u64::from(a) + u64::from(b) + u64::from(MIN_BODY_TWIPS)
        };
        if fits(page.w, left, right) && fits(page.h, top, bottom) {
            (page.top, page.right, page.bottom, page.left) = (top, right, bottom, left);
        }
        page.header = header.unwrap_or(page.top.min(HEADER_TWIPS));
        page.footer = footer.unwrap_or(page.bottom.min(HEADER_TWIPS));
        page
    }
}

/// A length on the page in twips (#1944): non-negative and at most
/// [`MAX_TWIPS`], else `None`. `em` and `rem` count from the base size.
fn page_twips(len: css::Len) -> Option<u32> {
    let t = (len.points(css::BASE_PT)? * 20.0).round();
    (0.0..=f64::from(MAX_TWIPS))
        .contains(&t)
        .then_some(t as u32)
}

/// A `size` value (#1944): its width and height, each `None` when it is
/// ignored (a length outside 1"..22"); `auto` is the default page's.
/// `None` when it cannot be read.
fn page_size(value: &str) -> Option<(Option<u32>, Option<u32>)> {
    let v = value.to_ascii_lowercase();
    let toks: Vec<&str> = v.split_ascii_whitespace().collect();
    let side = |t: u32| (MIN_PAGE_TWIPS..=MAX_TWIPS).contains(&t).then_some(t);
    match toks.as_slice() {
        ["auto"] => return Some((Some(PAGE_W_TWIPS), Some(PAGE_H_TWIPS))),
        [a] | [a, _] if css::length(a).is_some() => {
            let mut sides = Vec::with_capacity(2);
            for t in &toks {
                let len = css::length(t).filter(|l| l.unit != css::Unit::Percent)?;
                // Out of range (negative too) is ignored, not unreadable.
                sides.push(page_twips(len).and_then(side));
            }
            let w = sides[0];
            let h = sides.get(1).copied().unwrap_or(w);
            return Some((w, h));
        }
        _ => {}
    }
    // A paper keyword and an orientation, either or both, in any order.
    let mut paper: Option<(u32, u32)> = None;
    let mut orient: Option<bool> = None;
    for t in &toks {
        if let Some(p) = paper_size(t) {
            if paper.replace(p).is_some() {
                return None;
            }
        } else if *t == "portrait" || *t == "landscape" {
            if orient.replace(*t == "landscape").is_some() {
                return None;
            }
        } else {
            return None;
        }
    }
    if toks.is_empty() {
        return None;
    }
    let (w, h) = paper.unwrap_or((PAGE_W_TWIPS, PAGE_H_TWIPS));
    let (short, long) = (w.min(h), w.max(h));
    Some(match orient {
        Some(true) => (Some(long), Some(short)),
        Some(false) => (Some(short), Some(long)),
        None => (Some(w), Some(h)),
    })
}

/// A CSS paper keyword's portrait size in twips (#1944).
fn paper_size(name: &str) -> Option<(u32, u32)> {
    let mm = |w: f64, h: f64| {
        let t = |v: f64| (v * 1440.0 / 25.4).round() as u32;
        (t(w), t(h))
    };
    Some(match name {
        "letter" => (12_240, 15_840),
        "legal" => (12_240, 20_160),
        "ledger" => (15_840, 24_480),
        "a3" => mm(297.0, 420.0),
        "a4" => mm(210.0, 297.0),
        "a5" => mm(148.0, 210.0),
        "b4" => mm(250.0, 353.0),
        "b5" => mm(176.0, 250.0),
        _ => return None,
    })
}

/// The final section of an HTML import (#1928): `page`, the page the
/// reader laid the body out for, with every `w:pgMar` attribute (an
/// imported page has no header or footer; no gutter), and
/// `w:orient="landscape"` when it is wider than tall (#1944).
pub(crate) fn page_section(page: &Page) -> crate::model::SectionProperties {
    let Page {
        w,
        h,
        top,
        right,
        bottom,
        left,
        header,
        footer,
    } = *page;
    let orient = if w > h { " w:orient=\"landscape\"" } else { "" };
    crate::model::SectionProperties {
        raw: format!(
            "<w:sectPr><w:pgSz w:w=\"{w}\" w:h=\"{h}\"{orient}/>\
             <w:pgMar w:top=\"{top}\" w:right=\"{right}\" w:bottom=\"{bottom}\" \
             w:left=\"{left}\" w:header=\"{header}\" w:footer=\"{footer}\" \
             w:gutter=\"0\"/></w:sectPr>"
        ),
        property_change: None,
    }
}

/// The deepest table nesting kept; a table deeper than this joins its
/// enclosing cell's text, as every nested table did before #1878.
pub(crate) const MAX_TABLE_NESTING: usize = 8;

/// A declared width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Width {
    Twips(u32),
    /// A percentage, 0..=100 or more.
    Pct(f64),
}

/// A cell's vertical alignment (`w:vAlign`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum VAlign {
    Top,
    /// A browser's default, `middle`.
    #[default]
    Center,
    Bottom,
}

impl VAlign {
    fn val(self) -> &'static str {
        match self {
            VAlign::Top => "top",
            VAlign::Center => "center",
            VAlign::Bottom => "bottom",
        }
    }
}

/// What a `<table>` says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TableSpec {
    pub width: Option<Width>,
    /// CSS `max-width` (#1894): a width above it, or the room an auto
    /// table is laid out in, is cut to it.
    pub max_width: Option<Width>,
    /// The outer lines it draws (#1894), top, right, bottom, left: its CSS
    /// border over the `border` / `frame` / `bordercolor` hints.
    pub outer: [Option<ParBorder>; 4],
    /// The lines between its cells, `insideH` and `insideV` (#1894): the
    /// `border` / `rules` attributes' 1 px lines in the table's computed
    /// top and left border colours.
    pub inside: [Option<ParBorder>; 2],
    /// `border-collapse: collapse` (#1894, also `rules`' hint): no
    /// `tblCellSpacing`.
    pub collapse: bool,
    /// `cellpadding` / `cellspacing`, in px.
    pub cellpadding: Option<u32>,
    pub cellspacing: Option<u32>,
    /// `w:jc`, `center` or `right`; `None` is the left default (#1922):
    /// a float `align=right` (`align=left` is none), else the table's auto
    /// side margins (`<table align=center>` is that hint), else its
    /// container's `-webkit-` alignment. With a [`Room`], its place in
    /// the room, settled once it is laid out ([`layout`]).
    pub jc: Option<&'static str>,
    /// An outermost table in a centred fixed-width block (#2114), or a
    /// card in the body (#1940): the room it is laid out in, instead of
    /// the body.
    pub room: Option<Room>,
}

/// The content box of the centred fixed-width block an outermost table is
/// in (#2114, #1930 plan-2), or the room the blocks around a body card and
/// its own side margins leave (#1940), in twips from the body's left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Room {
    pub indent: u32,
    pub width: u32,
    /// The room is centred in the body (equal indents on both sides).
    pub centred: bool,
    /// The body's width, which Word resolves a `pct` `tblW` against
    /// (#2151 r1).
    pub body: u32,
    /// An indented block's room (#2151 r2): a percentage width is written
    /// as its share of the body, so a flow view fits it as the block's
    /// table. A centred fixed-width box's or a body card's room keeps it
    /// as the twips it is, so the box's width stands.
    pub body_share: bool,
}

/// What a `<td>` / `<th>` says about itself, its row and its table.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CellSpec {
    /// 1..=1000.
    pub colspan: u32,
    /// 0 reaches the table's last row.
    pub rowspan: u32,
    pub width: Option<Width>,
    /// `RRGGBB`: the cell's own background, else its row's, row group's or
    /// table's.
    pub fill: Option<String>,
    pub valign: VAlign,
    /// CSS `padding`, in twips, as top, right, bottom, left.
    pub padding: [Option<i32>; 4],
    /// Its own CSS border lines (#1894), top, right, bottom, left.
    pub borders: [Option<ParBorder>; 4],
    /// Its own `height` (#1894), in twips: its content box.
    pub height: Option<u32>,
    /// Its row's `height` (#1894), in twips: the whole row.
    pub row_height: Option<u32>,
}

impl Default for CellSpec {
    fn default() -> Self {
        CellSpec {
            colspan: 1,
            rowspan: 1,
            width: None,
            fill: None,
            valign: VAlign::Center,
            padding: [None; 4],
            borders: [None; 4],
            height: None,
            row_height: None,
        }
    }
}

/// A fixed-width block with both side margins `auto` inside a cell
/// (#1930): centred in the cell's inner width once [`layout`] knows it, as
/// one in the body is centred in the page's body ([`Page::body`], #1944)
/// while it is read (#1922).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CellBox {
    /// Its content width, in twips.
    pub width: i32,
    /// The look's indents with its own margins, border and padding in, the
    /// enclosing boxes' centring not yet: what the body-level branch takes
    /// from the page's body ([`Page::body`], #1944) with the width.
    pub indent: i32,
    pub indent_right: i32,
}

/// What a paragraph ended in a cell, inside centred [`CellBox`]es, needs
/// for its centring at [`layout`] (#1930).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CellCentring {
    /// Its centred boxes, outermost first.
    pub boxes: Vec<CellBox>,
    /// Its look's indents, before `max(0)`: the paragraph's indent is the
    /// centred one's at least 0, as at body level.
    pub indent: i32,
    pub indent_right: i32,
    /// Per side with a line (left, right): how many of `boxes` the block
    /// drawing it is in, and where it is (`border_left_x` /
    /// `border_right_x`): it moves with those boxes' centring only.
    pub lines: [Option<(usize, i32)>; 2],
}

/// A left or right line's `w:space` (#1883): what is between it and the
/// text, `gap` twips from the line's place to the text, less the line's
/// width, in whole points up to Word's 31.
pub(crate) fn line_space(gap: i32, sz: u16) -> u16 {
    let gap = f64::from(gap) - f64::from(sz) * 2.5;
    (gap / 20.0).round().clamp(0.0, 31.0) as u16
}

/// A cell as read: its blocks, with a placeholder at each index a nested
/// table holds (`nested`), laid out with the table.
#[derive(Debug, Default)]
pub(crate) struct RawCell {
    pub spec: CellSpec,
    pub blocks: Vec<Block>,
    pub nested: Vec<(usize, RawTable)>,
    /// The paragraphs at these indexes are in centred boxes (#1930).
    pub boxes: Vec<(usize, CellCentring)>,
}

impl RawCell {
    /// Take `other`'s blocks, nested tables and centred paragraphs after
    /// this cell's own (a cell with no room in its row).
    pub(crate) fn absorb(&mut self, other: RawCell) {
        let base = self.blocks.len();
        self.blocks.extend(other.blocks);
        self.nested
            .extend(other.nested.into_iter().map(|(i, t)| (base + i, t)));
        self.boxes
            .extend(other.boxes.into_iter().map(|(i, c)| (base + i, c)));
    }
}

/// Centre the paragraphs `boxes` name in `blocks` in `inner` twips (#1930):
/// each box in the room its enclosing ones' centring leaves, as the body
/// does in the page's body ([`Page::body`], #1944; #1922). A box wider than
/// its room gets nothing.
fn centre_in_cell(blocks: &mut [Block], boxes: Vec<(usize, CellCentring)>, inner: u32) {
    let clamp = |v: i64| v.clamp(-i64::from(MAX_TWIPS), i64::from(MAX_TWIPS));
    for (at, c) in boxes {
        let Some(Block::Paragraph(p)) = blocks.get_mut(at) else {
            continue;
        };
        // What the boxes up to each one move the text by.
        let mut moved = Vec::with_capacity(c.boxes.len() + 1);
        moved.push(0i64);
        let mut extra = 0i64;
        for b in &c.boxes {
            let room = i64::from(inner)
                - clamp(i64::from(b.indent) + extra)
                - clamp(i64::from(b.indent_right) + extra)
                - i64::from(b.width.max(0));
            extra = clamp(extra + (room / 2).max(0));
            moved.push(extra);
        }
        if extra == 0 {
            continue;
        }
        let at_least_0 = |v: i32| i64::from(v.max(0));
        let grow = |now: i32, raw: i32| {
            let want = clamp(i64::from(raw) + extra).max(0);
            clamp(i64::from(now) + want - at_least_0(raw)) as i32
        };
        p.props.indent = grow(p.props.indent, c.indent);
        p.props.indent_right = grow(p.props.indent_right, c.indent_right);
        // The model's indent is no longer 0: the import's explicit zero
        // goes (the writer writes the moved one).
        p.props.shadow.retain(|x| x != super::html::ZERO_IND);
        // A line stays at its block's place, which moved with the boxes
        // that block is in only.
        let indents = [p.props.indent, p.props.indent_right];
        let sides = [&mut p.props.borders.left, &mut p.props.borders.right];
        for ((line, side), text) in c.lines.iter().zip(sides).zip(indents) {
            if let (Some((n, x)), Some(b)) = (line, side.as_mut()) {
                let shift = moved.get(*n).copied().unwrap_or(extra);
                let gap = clamp(i64::from(text) - i64::from(*x) - shift) as i32;
                b.space = line_space(gap, b.sz);
            }
        }
    }
}

/// A table as read: its spec, its rows of cells and where its row groups
/// start.
#[derive(Debug, Default)]
pub(crate) struct RawTable {
    pub spec: TableSpec,
    pub rows: Vec<Vec<RawCell>>,
    /// Row indexes where a row group (`thead`, `tbody`, `tfoot`, or the rows
    /// after one) starts, ascending: a `rowspan` ends with its group.
    pub groups: Vec<usize>,
    /// Each grid column's `<col>` / `<colgroup>` width, in order (#1894).
    pub cols: Vec<Option<Width>>,
}

/// The placeholder a nested table holds in its cell until [`layout`].
pub(crate) fn placeholder() -> Block {
    Block::Raw(String::new())
}

/// Whether `b` is a nested table's [`placeholder`].
pub(crate) fn is_placeholder(b: &Block) -> bool {
    matches!(b, Block::Raw(s) if s.is_empty())
}

/// px as twips, saturating.
pub(crate) fn px_twips(px: u32) -> u32 {
    px.saturating_mul(15).min(MAX_TWIPS)
}

/// One placed cell.
struct Placed {
    cell: RawCell,
    /// Its first row (#1894).
    row: usize,
    col: usize,
    span: usize,
    /// Rows it covers, itself included (≥ 1).
    rows: usize,
}

/// What a slot of the grid holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Free,
    /// The first column of placed cell `i` in its own row.
    Start(usize),
    /// Covered by placed cell `i` (its other columns, or a row below).
    Covered(usize),
}

/// Lay `t` out in `avail` twips, its nested tables in their cells' widths.
/// A table with a [`Room`] (whose width its caller passes as `avail`) is
/// then placed in it ([`place_in_room`]).
pub(crate) fn layout(t: RawTable, avail: u32, budget: &Budget) -> Table {
    layout_in(t, avail, true, budget)
}

/// [`layout`], `declared` saying whether the page declares `avail` (the
/// body, a room, a cell whose width it declares): a centred box is centred
/// only in a cell whose width the page declares (#1930), as an auto table a
/// browser shrinks to its content.
fn layout_in(t: RawTable, avail: u32, declared: bool, budget: &Budget) -> Table {
    let RawTable {
        mut spec,
        rows,
        groups,
        cols,
    } = t;
    let avail = avail.clamp(MIN_COL, MAX_TWIPS);
    // Its own width (a card's always), or the room it is placed in.
    let table_declared = match spec.width {
        Some(Width::Twips(_)) => true,
        Some(Width::Pct(_)) => declared,
        None => spec.room.is_some(),
    };
    // A width the page gives the cell: the table's, the cell's own, or
    // `<col>`s over all its columns (a percentage of the table's).
    let known = |w: Option<Width>| match w {
        Some(Width::Twips(_)) => true,
        Some(Width::Pct(_)) => table_declared,
        None => false,
    };
    let cell_declared = |p: &Placed| {
        table_declared
            || known(p.cell.spec.width)
            || (p.col + p.span <= cols.len()
                && cols[p.col..p.col + p.span].iter().all(|w| known(*w)))
    };
    // `max-width` (#1894): a wider declared width is cut to it; an auto
    // table is laid out in no more than it.
    let mut room_w = avail;
    if let Some(max) = spec.max_width {
        let cap = resolve(max, avail);
        match spec.width {
            Some(w) if resolve(w, avail) > cap => spec.width = Some(Width::Twips(cap)),
            Some(_) => {}
            None => room_w = avail.min(cap),
        }
    }
    let nrows = rows.len();
    let (placed, slots) = place(rows, nrows, &groups);
    let ncols = slots
        .iter()
        .map(|r| {
            r.iter()
                .rposition(|s| *s != Slot::Free)
                .map_or(0, |c| c + 1)
        })
        .max()
        .unwrap_or(0)
        .max(1);
    let grid = column_widths(&spec, &placed, &cols, ncols, room_w);
    let span_w = |col: usize, span: usize| -> u32 { grid[col..col + span].iter().sum() };
    let pad = spec.cellpadding.map(px_twips);

    // Each placed cell's tcPr (the continuation cells under it share it,
    // vMerge aside) and its blocks, nested tables laid out in its width.
    let mut tcprs = Vec::with_capacity(placed.len());
    let mut cells: Vec<Option<Vec<Block>>> = Vec::with_capacity(placed.len());
    let reach: Vec<usize> = placed.iter().map(|p| p.rows).collect();
    // Each placed cell's first row (#1894), kept for its merged slots
    // below, once the cells are taken.
    let start_row: Vec<usize> = placed.iter().map(|p| p.row).collect();
    // A row's height (#1894): at least its own `height` and each cell's
    // that starts in it and spans no other row, with its top and bottom
    // margins (a cell's height is its content box, Word's the row's).
    let mut heights: Vec<u32> = vec![0; nrows];
    for p in &placed {
        let r = p.row;
        let spec_pad = p.cell.spec.padding;
        let margin = |side: usize| spec_pad[side].map(|t| t.max(0) as u32).or(pad).unwrap_or(0);
        if let Some(h) = p.cell.spec.row_height {
            heights[r] = heights[r].max(h.min(MAX_TWIPS));
        }
        if let Some(h) = p.cell.spec.height.filter(|_| p.rows == 1) {
            let h = h.saturating_add(margin(0)).saturating_add(margin(2));
            heights[r] = heights[r].max(h.min(MAX_TWIPS));
        }
    }
    // Where the table draws an outer line and its cells are not spaced
    // apart, Word draws a cell's own line on that edge instead of the
    // table's: as in a browser's collapsed borders, the wider wins, a cell
    // a tie (#1894).
    let spaced = !spec.collapse && spec.cellspacing.is_some_and(|n| n > 0);
    let edge_keeps = |side: usize, b: Option<ParBorder>| -> Option<ParBorder> {
        let b = b?;
        match spec.outer[side] {
            Some(t) if !spaced && t.sz > b.sz => None,
            _ => Some(b),
        }
    };
    for p in &placed {
        let w = span_w(p.col, p.span);
        let mut borders = p.cell.spec.borders;
        let first = p.row;
        if first == 0 {
            borders[0] = edge_keeps(0, borders[0]);
        }
        if first + p.rows >= nrows {
            borders[2] = edge_keeps(2, borders[2]);
        }
        if p.col == 0 {
            borders[3] = edge_keeps(3, borders[3]);
        }
        if p.col + p.span >= ncols {
            borders[1] = edge_keeps(1, borders[1]);
        }
        tcprs.push(TcPr {
            width: w,
            span: p.span as u32,
            fill: p.cell.spec.fill.clone(),
            valign: p.cell.spec.valign,
            padding: p.cell.spec.padding,
            borders,
        });
        cells.push(None);
    }
    for (i, p) in placed.into_iter().enumerate() {
        let spec_pad = p.cell.spec.padding;
        let margin = |side: usize| spec_pad[side].map(|t| t.max(0) as u32).or(pad).unwrap_or(0);
        let inner = tcprs[i]
            .width
            .saturating_sub(margin(1) + margin(3))
            .max(MIN_COL);
        let declared = cell_declared(&p);
        let RawCell {
            mut blocks,
            nested,
            boxes,
            ..
        } = p.cell;
        if declared {
            centre_in_cell(&mut blocks, boxes, inner);
        }
        for (at, raw) in nested {
            let table = layout_in(raw, inner, declared, budget);
            if let Some(b) = blocks.get_mut(at) {
                *b = Block::Table(table);
            }
        }
        // Word wants a cell to end in a paragraph (also after a table).
        if !matches!(blocks.last(), Some(Block::Paragraph(_))) {
            blocks.push(Block::Paragraph(Paragraph::default()));
        }
        cells[i] = Some(blocks);
    }

    let mut out_rows = Vec::with_capacity(nrows);
    for (r, row_slots) in slots.iter().enumerate() {
        let mut row = Row::default();
        if heights[r] > 0 {
            row.raw_props.push(format!(
                "<w:trPr><w:trHeight w:val=\"{}\" w:hRule=\"atLeast\"/></w:trPr>",
                heights[r]
            ));
        }
        let mut c = 0;
        while c < ncols {
            let slot = row_slots.get(c).copied().unwrap_or(Slot::Free);
            let (cell, step) = match slot {
                Slot::Start(i) => {
                    let p = &tcprs[i];
                    let merge = if reach[i] > 1 {
                        VMerge::Restart
                    } else {
                        VMerge::None
                    };
                    // A merged cell's bottom line is under its last row.
                    let mut b = p.borders;
                    if reach[i] > 1 {
                        b[2] = None;
                    }
                    let blocks = cells[i].take().unwrap_or_default();
                    (make_cell(p, merge, b, blocks), p.span as usize)
                }
                Slot::Covered(i) => {
                    let p = &tcprs[i];
                    // Its sides on every row, its bottom on its last.
                    let last = start_row[i] + reach[i] == r + 1;
                    let b = [
                        None,
                        p.borders[1],
                        p.borders[2].filter(|_| last),
                        p.borders[3],
                    ];
                    let blocks = vec![Block::Paragraph(Paragraph::default())];
                    (make_cell(p, VMerge::Continue, b, blocks), p.span as usize)
                }
                Slot::Free => {
                    let p = TcPr {
                        width: grid[c],
                        span: 1,
                        fill: None,
                        valign: VAlign::Center,
                        padding: [None; 4],
                        borders: [None; 4],
                    };
                    let blocks = vec![Block::Paragraph(Paragraph::default())];
                    (make_cell(&p, VMerge::None, [None; 4], blocks), 1)
                }
            };
            let heap = cell.raw_tcpr.as_ref().map_or(0, String::len);
            if !budget.keep::<Cell>(heap) {
                break;
            }
            row.cells.push(cell);
            c += step.max(1);
        }
        out_rows.push(row);
    }

    let mut table = Table {
        grid,
        rows: out_rows,
        ..Table::default()
    };
    let indent = match spec.room {
        Some(room) => {
            // Word measures the indent to the first cell's text: its own
            // left padding (`tcMar`) if it has one, else the table's
            // `cellpadding`, else Word's 108 (#2151).
            table.raw_tblpr = Some(tblpr(&spec, None));
            let margin = u32::try_from(crate::table::first_cell_left_margin(&table)).unwrap_or(0);
            place_in_room(&mut spec, room, &table.grid, avail, margin)
        }
        None => None,
    };
    table.raw_tblpr = Some(tblpr(&spec, indent));
    table
}

/// Place a table laid out in `room` (#2114, #1930 plan-2) on the body: its
/// percentage width as the twips it is in the room (Word resolves `pct`
/// against the body and would stretch it back), or in an indented block's
/// room ([`Room::body_share`]) as the share of the body those twips are
/// (Word gets the same width, and a flow view fits a `pct` table to its
/// width, #1895, where `dxa` would keep it wide: #2151 r1, r2), and
/// `jc center` when it
/// fills the room exactly or is centred in it and the room is centred in
/// the body (its own `align` then moves nothing). Otherwise (a wider table
/// too, which a browser starts at the room's left) no `jc` and the
/// `w:tblInd` (returned) that puts its left edge where the browser does:
/// at the room's left, right or middle. A package with no
/// `compatibilityMode` (the Markdown one) is laid out by Word in an older
/// mode, where `tblInd` is measured to the first cell's text: its left
/// cell margin (`cell_margin`, its own padding or the table's, #2151) is
/// added.
fn place_in_room(
    spec: &mut TableSpec,
    room: Room,
    grid: &[u32],
    avail: u32,
    cell_margin: u32,
) -> Option<u32> {
    if let Some(w @ Width::Pct(_)) = spec.width {
        let twips = resolve(w, avail);
        spec.width = Some(if room.body_share {
            Width::Pct(f64::from(twips) * 100.0 / f64::from(room.body.max(1)))
        } else {
            Width::Twips(twips)
        });
    }
    let width: u32 = grid.iter().fold(0u32, |a, &w| a.saturating_add(w));
    let place = spec.jc.take();
    // Only a table that fits (r2 m1): a wider one is over-constrained, its
    // auto margins 0, so it starts at the room's left and overflows right.
    if room.centred && (width == room.width || (width < room.width && place == Some("center"))) {
        spec.jc = Some("center");
        return None;
    }
    let free = room.width.saturating_sub(width);
    let offset = room.indent
        + match place {
            Some("center") => free / 2,
            Some("right") => free,
            _ => 0,
        };
    Some(offset.saturating_add(cell_margin).min(MAX_TWIPS))
}

/// Place every cell in the first free slot of its row from the last one
/// placed (a browser's table model), spanning `colspan` free columns, at
/// most to [`MAX_COLUMNS`], and `rowspan` rows (0 or past the end: to the
/// last row of its row group, as a browser's table model clips it). A cell with no free slot left joins the row's previous cell,
/// else the cell that covers the row's last slot.
fn place(rows: Vec<Vec<RawCell>>, nrows: usize, groups: &[usize]) -> (Vec<Placed>, Vec<Vec<Slot>>) {
    let mut slots: Vec<Vec<Slot>> = (0..nrows).map(|_| Vec::new()).collect();
    let mut placed: Vec<Placed> = Vec::new();
    for (r, row) in rows.into_iter().enumerate() {
        // The end of row `r`'s group.
        let group_end = groups
            .iter()
            .copied()
            .find(|&g| g > r)
            .unwrap_or(nrows)
            .min(nrows);
        let mut cursor = 0;
        let mut last: Option<usize> = None;
        for cell in row {
            let free =
                |slots: &[Vec<Slot>], c: usize| slots[r].get(c).is_none_or(|s| *s == Slot::Free);
            while cursor < MAX_COLUMNS && !free(&slots, cursor) {
                cursor += 1;
            }
            if cursor >= MAX_COLUMNS {
                let owner = last.or_else(|| match slots[r].last() {
                    Some(Slot::Start(i) | Slot::Covered(i)) => Some(*i),
                    _ => None,
                });
                if let Some(i) = owner {
                    placed[i].cell.absorb(cell);
                }
                continue;
            }
            let want = (cell.spec.colspan.max(1) as usize).min(MAX_COLUMNS - cursor);
            let mut span = 0;
            while span < want && free(&slots, cursor + span) {
                span += 1;
            }
            let reach = match cell.spec.rowspan {
                0 => group_end - r,
                n => (n as usize).min(group_end - r),
            };
            let i = placed.len();
            for (k, row_slots) in slots[r..r + reach].iter_mut().enumerate() {
                if row_slots.len() < cursor + span {
                    row_slots.resize(cursor + span, Slot::Free);
                }
                for (j, s) in row_slots[cursor..cursor + span].iter_mut().enumerate() {
                    *s = if k == 0 && j == 0 {
                        Slot::Start(i)
                    } else {
                        Slot::Covered(i)
                    };
                }
            }
            placed.push(Placed {
                cell,
                row: r,
                col: cursor,
                span,
                rows: reach,
            });
            last = Some(i);
            cursor += span;
        }
    }
    (placed, slots)
}

/// The grid: a column's width is the widest colspan-1 cell's in it (a
/// percentage of the table's width, percentages scaled down to 100 in all);
/// a spanned cell's width spreads what its columns lack over its columns
/// without one; the rest share what is left of the table's width, each at
/// least [`MIN_COL`], except that a width-less column no colspan-1 cell sits
/// in takes [`MIN_COL`] while another width-less column has a cell of its own
/// (#2249). A table's width is its own, else the columns' when
/// every one has a width, else `avail`; declared columns narrower than a
/// declared table grow to it, and columns wider than the table widen it.
fn column_widths(
    spec: &TableSpec,
    placed: &[Placed],
    cols: &[Option<Width>],
    ncols: usize,
    avail: u32,
) -> Vec<u32> {
    let declared = spec.width.map(|w| resolve(w, avail));
    let reference = declared.unwrap_or(avail);
    let single: Vec<&Placed> = placed.iter().filter(|p| p.span == 1).collect();
    // A `<col>`'s width is its column's as a colspan-1 cell's would be
    // (#1894); columns past the grid add none.
    let cols = &cols[..cols.len().min(ncols)];
    let widths = || {
        cols.iter()
            .enumerate()
            .filter_map(|(c, w)| Some((c, (*w)?)))
            .chain(
                single
                    .iter()
                    .filter_map(|p| Some((p.col, p.cell.spec.width?))),
            )
    };
    // Percent columns, scaled to 100 in all.
    let mut pct = vec![0.0f64; ncols];
    for (c, w) in widths() {
        if let Width::Pct(v) = w {
            pct[c] = pct[c].max(v.max(0.0));
        }
    }
    let total: f64 = pct.iter().sum();
    if total > 100.0 {
        for v in &mut pct {
            *v *= 100.0 / total;
        }
    }
    let mut col: Vec<Option<u32>> = vec![None; ncols];
    for (c, v) in pct.iter().enumerate() {
        if *v > 0.0 {
            col[c] = Some(resolve(Width::Pct(*v), reference));
        }
    }
    for (c, w) in widths() {
        if let Width::Twips(t) = w {
            col[c] = Some(col[c].unwrap_or(0).max(t));
        }
    }
    for p in placed.iter().filter(|p| p.span > 1) {
        let Some(w) = p.cell.spec.width else { continue };
        let w = resolve(w, reference);
        let range = p.col..p.col + p.span;
        let known: u32 = col[range.clone()].iter().flatten().sum();
        let Some(lack) = w.checked_sub(known).filter(|&l| l > 0) else {
            continue;
        };
        let unknown: Vec<usize> = range.clone().filter(|&c| col[c].is_none()).collect();
        let targets: Vec<usize> = if unknown.is_empty() {
            range.collect()
        } else {
            unknown
        };
        let share = lack / targets.len() as u32;
        let extra = lack % targets.len() as u32;
        for (k, &c) in targets.iter().enumerate() {
            let add = share + u32::from((k as u32) < extra);
            col[c] = Some(col[c].unwrap_or(0) + add);
        }
    }
    // A column no colspan-1 cell sits in (only a span or an empty slot
    // reaches it, such as a card's `colspan=2` footer under a one-cell row)
    // holds nothing of its own: a browser and Word give it next to no width,
    // so the columns with content share the rest (#2249). When every column
    // left is bare, they share it as before.
    // A span made only of bare columns (`<td colspan=2>` beside a cell,
    // never split by another row) is that cell's own room: its columns are
    // not pinned.
    let has_own: Vec<bool> = (0..ncols)
        .map(|c| single.iter().any(|p| p.col == c))
        .collect();
    if (0..ncols).any(|c| col[c].is_none() && has_own[c]) {
        let spans: Vec<std::ops::Range<usize>> = placed
            .iter()
            .filter(|p| p.span > 1)
            .map(|p| p.col..(p.col + p.span).min(ncols))
            .collect();
        let pin: Vec<bool> = (0..ncols)
            .map(|c| {
                col[c].is_none()
                    && !has_own[c]
                    && spans
                        .iter()
                        .filter(|r| r.contains(&c))
                        .all(|r| r.clone().any(|k| has_own[k] || col[k].is_some()))
            })
            .collect();
        for (c, pinned) in pin.into_iter().enumerate() {
            if pinned {
                col[c] = Some(MIN_COL);
            }
        }
    }
    let known: u32 = col.iter().flatten().sum();
    let unknown = col.iter().filter(|c| c.is_none()).count() as u32;
    let width = match declared {
        Some(w) => w,
        None if unknown == 0 => known,
        None => avail,
    };
    let mut grid: Vec<u32>;
    if let Some(each) = width.saturating_sub(known).checked_div(unknown) {
        let left = width.saturating_sub(known);
        let share = each.max(MIN_COL);
        let mut extra = if each >= MIN_COL { left % unknown } else { 0 };
        grid = col
            .iter()
            .map(|c| match c {
                Some(w) => (*w).max(MIN_COL),
                None => {
                    let add = u32::from(extra > 0);
                    extra = extra.saturating_sub(1);
                    share + add
                }
            })
            .collect();
    } else {
        grid = col.iter().map(|c| c.unwrap_or(0).max(MIN_COL)).collect();
        let sum: u32 = grid.iter().sum();
        if declared.is_some() && sum < width {
            grid = scale(&grid, width);
        }
    }
    let sum: u64 = grid.iter().map(|&w| u64::from(w)).sum();
    if sum > u64::from(MAX_TWIPS) {
        grid = scale(&grid, MAX_TWIPS);
    }
    grid
}

/// `grid` scaled to sum to `to`, each column at least [`MIN_COL`], the
/// rounding left over going to the last column.
fn scale(grid: &[u32], to: u32) -> Vec<u32> {
    let sum: u64 = grid.iter().map(|&w| u64::from(w)).sum();
    if sum == 0 {
        return grid.to_vec();
    }
    let mut out: Vec<u32> = grid
        .iter()
        .map(|&w| ((u64::from(w) * u64::from(to) / sum) as u32).max(MIN_COL))
        .collect();
    let got: u64 = out.iter().map(|&w| u64::from(w)).sum();
    if let Some(last) = out.last_mut() {
        if got < u64::from(to) {
            *last += (u64::from(to) - got) as u32;
        }
    }
    out
}

/// A width in twips, a percentage of `of`.
fn resolve(w: Width, of: u32) -> u32 {
    let t = match w {
        Width::Twips(t) => t,
        Width::Pct(p) => (f64::from(of) * p.clamp(0.0, 100.0) / 100.0).round() as u32,
    };
    t.clamp(MIN_COL, MAX_TWIPS)
}

/// A cell's properties, as written.
struct TcPr {
    width: u32,
    span: u32,
    fill: Option<String>,
    valign: VAlign,
    padding: [Option<i32>; 4],
    /// The cell's own lines (#1894), top, right, bottom, left, those the
    /// table's frame outranks taken out; each slot of a merged cell writes
    /// what of them is its own.
    borders: [Option<ParBorder>; 4],
}

fn make_cell(p: &TcPr, merge: VMerge, borders: [Option<ParBorder>; 4], blocks: Vec<Block>) -> Cell {
    Cell {
        grid_span: p.span.max(1),
        v_merge: merge,
        blocks,
        raw_tcpr: Some(tcpr(p, merge, &borders)),
        ..Cell::default()
    }
}

/// `w:tcPr`, its children in schema order: tcW, gridSpan, vMerge,
/// tcBorders, shd, tcMar, vAlign.
fn tcpr(p: &TcPr, merge: VMerge, borders: &[Option<ParBorder>; 4]) -> String {
    let mut s = format!("<w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>", p.width);
    if p.span > 1 {
        s.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", p.span));
    }
    match merge {
        VMerge::None => {}
        VMerge::Restart => s.push_str("<w:vMerge w:val=\"restart\"/>"),
        VMerge::Continue => s.push_str("<w:vMerge/>"),
    }
    // CSS's top, right, bottom, left as tcBorders' top, left, bottom, right.
    let lines = border_lines(&[
        ("w:top", borders[0]),
        ("w:left", borders[3]),
        ("w:bottom", borders[2]),
        ("w:right", borders[1]),
    ]);
    if !lines.is_empty() {
        s.push_str(&format!("<w:tcBorders>{lines}</w:tcBorders>"));
    }
    if let Some(fill) = &p.fill {
        s.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>"
        ));
    }
    // CSS's top, right, bottom, left as tcMar's top, left, bottom, right.
    let mar = margins(&[
        ("w:top", p.padding[0]),
        ("w:left", p.padding[3]),
        ("w:bottom", p.padding[2]),
        ("w:right", p.padding[1]),
    ]);
    if !mar.is_empty() {
        s.push_str(&format!("<w:tcMar>{mar}</w:tcMar>"));
    }
    s.push_str(&format!("<w:vAlign w:val=\"{}\"/>", p.valign.val()));
    s.push_str("</w:tcPr>");
    s
}

/// The border elements of the sides that draw a line (#1894).
fn border_lines(sides: &[(&str, Option<ParBorder>)]) -> String {
    sides
        .iter()
        .filter_map(|(name, b)| b.as_ref().map(|b| crate::serialize::pbdr_side_xml(name, b)))
        .collect()
}

/// The margin elements given, in twips (a negative one is 0).
fn margins(sides: &[(&str, Option<i32>)]) -> String {
    let mut s = String::new();
    for (name, v) in sides {
        if let Some(v) = v {
            let v = (*v).clamp(0, MAX_TWIPS as i32);
            s.push_str(&format!("<{name} w:w=\"{v}\" w:type=\"dxa\"/>"));
        }
    }
    s
}

/// `w:tblPr`, its children in schema order: tblW, jc, tblCellSpacing,
/// tblInd (`indent`), tblBorders, tblCellMar.
fn tblpr(spec: &TableSpec, indent: Option<u32>) -> String {
    let mut s = String::from("<w:tblPr>");
    match spec.width {
        Some(Width::Twips(t)) => s.push_str(&format!(
            "<w:tblW w:w=\"{}\" w:type=\"dxa\"/>",
            t.clamp(MIN_COL, MAX_TWIPS)
        )),
        Some(Width::Pct(p)) => s.push_str(&format!(
            "<w:tblW w:w=\"{}\" w:type=\"pct\"/>",
            (p.clamp(0.0, 100.0) * 50.0).round() as u32
        )),
        None => s.push_str("<w:tblW w:w=\"0\" w:type=\"auto\"/>"),
    }
    if let Some(jc) = spec.jc {
        s.push_str(&format!("<w:jc w:val=\"{jc}\"/>"));
    }
    // Collapsed borders have no spacing between cells (#1894).
    if let Some(n) = spec.cellspacing.filter(|&n| n > 0 && !spec.collapse) {
        // Word's spacing is on each side of a cell: half the HTML gap
        // (unmeasured against Word).
        s.push_str(&format!(
            "<w:tblCellSpacing w:w=\"{}\" w:type=\"dxa\"/>",
            px_twips(n) / 2
        ));
    }
    if let Some(ind) = indent {
        s.push_str(&format!("<w:tblInd w:w=\"{ind}\" w:type=\"dxa\"/>"));
    }
    // CSS's top, right, bottom, left as top, left, bottom, right (#1894).
    let lines = border_lines(&[
        ("w:top", spec.outer[0]),
        ("w:left", spec.outer[3]),
        ("w:bottom", spec.outer[2]),
        ("w:right", spec.outer[1]),
        ("w:insideH", spec.inside[0]),
        ("w:insideV", spec.inside[1]),
    ]);
    if !lines.is_empty() {
        s.push_str(&format!("<w:tblBorders>{lines}</w:tblBorders>"));
    }
    if let Some(px) = spec.cellpadding {
        let t = Some(px_twips(px) as i32);
        let mar = margins(&[("w:top", t), ("w:left", t), ("w:bottom", t), ("w:right", t)]);
        s.push_str(&format!("<w:tblCellMar>{mar}</w:tblCellMar>"));
    }
    s.push_str("</w:tblPr>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Inline, Run};

    fn text_cell(text: &str, spec: CellSpec) -> RawCell {
        RawCell {
            spec,
            blocks: vec![Block::Paragraph(Paragraph {
                content: vec![Inline::Run(Run {
                    text: text.into(),
                    ..Run::default()
                })],
                ..Paragraph::default()
            })],
            nested: Vec::new(),
            boxes: Vec::new(),
        }
    }

    /// A `single` automatic line of `sz` eighths, as `border=N` draws.
    fn line(sz: u16) -> ParBorder {
        ParBorder {
            kind: crate::model::BorderKind::Single,
            sz,
            space: 0,
            color: None,
            theme: None,
        }
    }

    fn spec(colspan: u32, rowspan: u32) -> CellSpec {
        CellSpec {
            colspan,
            rowspan,
            ..CellSpec::default()
        }
    }

    fn texts(t: &Table) -> Vec<Vec<(String, u32, VMerge)>> {
        t.rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| {
                        let text: String = c.blocks.iter().map(|b| b.plain_text()).collect();
                        (text, c.grid_span, c.v_merge)
                    })
                    .collect()
            })
            .collect()
    }

    fn lay(rows: Vec<Vec<RawCell>>) -> Table {
        layout(
            RawTable {
                spec: TableSpec::default(),
                rows,
                groups: Vec::new(),
                cols: Vec::new(),
            },
            BODY_TWIPS,
            &Budget::standard(),
        )
    }

    #[test]
    fn spans_place_cells_around_the_cells_above() {
        // a spans two columns; b spans two rows, so the second row's c goes
        // after it; the second row is padded to the grid.
        let t = lay(vec![
            vec![text_cell("a", spec(2, 1)), text_cell("b", spec(1, 2))],
            vec![text_cell("c", spec(1, 1))],
        ]);
        use VMerge::*;
        assert_eq!(
            texts(&t),
            [
                vec![("a".into(), 2, None), ("b".into(), 1, Restart)],
                vec![
                    ("c".into(), 1, None),
                    ("".into(), 1, None),
                    ("".into(), 1, Continue)
                ],
            ]
        );
        assert_eq!(t.grid.len(), 3);
        // A rowspan of 0 reaches the last row; past it, it is clipped.
        let t = lay(vec![
            vec![text_cell("a", spec(1, 0)), text_cell("b", spec(1, 9))],
            vec![text_cell("c", spec(1, 1))],
            vec![text_cell("d", spec(1, 1))],
        ]);
        assert_eq!(
            texts(&t),
            [
                vec![
                    ("a".into(), 1, Restart),
                    ("b".into(), 1, Restart),
                    ("".into(), 1, None)
                ],
                vec![
                    ("".into(), 1, Continue),
                    ("".into(), 1, Continue),
                    ("c".into(), 1, None)
                ],
                vec![
                    ("".into(), 1, Continue),
                    ("".into(), 1, Continue),
                    ("d".into(), 1, None)
                ],
            ]
        );
        // Continuations carry the restart cell's tcPr but `vMerge`.
        let restart = t.rows[0].cells[0].raw_tcpr.clone().unwrap();
        let cont = t.rows[1].cells[0].raw_tcpr.clone().unwrap();
        assert!(
            restart.contains("<w:vMerge w:val=\"restart\"/>"),
            "{restart}"
        );
        assert_eq!(
            cont,
            restart.replace("<w:vMerge w:val=\"restart\"/>", "<w:vMerge/>")
        );
    }

    #[test]
    fn a_cell_with_no_room_joins_its_neighbour() {
        let wide = |t: &str| text_cell(t, spec(1000, 1));
        let t = lay(vec![vec![wide("a"), wide("b")]]);
        assert_eq!(t.grid.len(), MAX_COLUMNS);
        assert_eq!(
            texts(&t),
            [vec![("ab".into(), MAX_COLUMNS as u32, VMerge::None)]]
        );
        // A row whose every slot a rowspan above takes: its cell joins the
        // covering cell.
        let t = lay(vec![
            vec![text_cell("a", spec(1000, 2))],
            vec![text_cell("b", spec(1, 1))],
        ]);
        assert_eq!(texts(&t)[0][0].0, "ab");
        assert_eq!(t.rows[1].cells.len(), 1);
        assert_eq!(t.rows[1].cells[0].v_merge, VMerge::Continue);
    }

    fn widths(spec_w: Option<Width>, cells: &[Option<Width>]) -> (Vec<u32>, String) {
        let row = cells
            .iter()
            .map(|w| {
                text_cell(
                    "x",
                    CellSpec {
                        width: *w,
                        ..CellSpec::default()
                    },
                )
            })
            .collect();
        let t = layout(
            RawTable {
                spec: TableSpec {
                    width: spec_w,
                    ..TableSpec::default()
                },
                rows: vec![row],
                groups: Vec::new(),
                cols: Vec::new(),
            },
            BODY_TWIPS,
            &Budget::standard(),
        );
        (t.grid, t.raw_tblpr.unwrap())
    }

    #[test]
    fn column_widths_follow_the_declared_ones() {
        use Width::*;
        // Known columns; the unknown one takes the rest of a 600px table.
        let (g, pr) = widths(Some(Twips(9000)), &[Some(Twips(3000)), None]);
        assert_eq!(g, [3000, 6000]);
        assert!(pr.contains("<w:tblW w:w=\"9000\" w:type=\"dxa\"/>"), "{pr}");
        // Every column known, no table width: their sum, tblW auto.
        let (g, pr) = widths(None, &[Some(Twips(3000)), Some(Twips(6000))]);
        assert_eq!(g, [3000, 6000]);
        assert!(pr.contains("<w:tblW w:w=\"0\" w:type=\"auto\"/>"), "{pr}");
        // Narrower than a declared table: scaled up to it.
        let (g, _) = widths(Some(Twips(9000)), &[Some(Twips(1500)), Some(Twips(1500))]);
        assert_eq!(g, [4500, 4500]);
        // Percent of the table; pct tblW in fiftieths.
        let (g, pr) = widths(Some(Pct(50.0)), &[Some(Pct(25.0)), None]);
        assert_eq!(g, [1474, 4421]);
        assert!(pr.contains("<w:tblW w:w=\"2500\" w:type=\"pct\"/>"), "{pr}");
        // No widths at all: the room shared evenly.
        let (g, _) = widths(None, &[None, None, None]);
        assert_eq!(g, [3930, 3930, 3930]);
    }

    #[test]
    fn over_full_widths_widen_the_grid() {
        use Width::*;
        // Percentages over 100 scale down to 100.
        let (g, _) = widths(Some(Twips(9000)), &[Some(Pct(80.0)), Some(Pct(80.0))]);
        assert_eq!(g, [4500, 4500]);
        // Fixed columns wider than the table: the grid is their sum, the
        // unknown column at its minimum; tblW stays as declared.
        let (g, pr) = widths(
            Some(Twips(3000)),
            &[Some(Twips(3000)), Some(Twips(3000)), None],
        );
        assert_eq!(g, [3000, 3000, MIN_COL]);
        assert!(pr.contains("<w:tblW w:w=\"3000\" w:type=\"dxa\"/>"), "{pr}");
        let (g, _) = widths(Some(Twips(3000)), &[Some(Twips(4000)), Some(Twips(4000))]);
        assert_eq!(g, [4000, 4000]);
    }

    #[test]
    fn a_spanned_cell_widens_the_columns_it_spans() {
        let t = lay(vec![
            vec![text_cell(
                "a",
                CellSpec {
                    colspan: 2,
                    width: Some(Width::Twips(6000)),
                    ..CellSpec::default()
                },
            )],
            vec![
                text_cell(
                    "b",
                    CellSpec {
                        width: Some(Width::Twips(1000)),
                        ..CellSpec::default()
                    },
                ),
                text_cell("c", spec(1, 1)),
            ],
        ]);
        assert_eq!(t.grid, [1000, 5000]);
        let tcw = t.rows[0].cells[0].raw_tcpr.clone().unwrap();
        assert!(
            tcw.contains("<w:tcW w:w=\"6000\" w:type=\"dxa\"/>"),
            "{tcw}"
        );
    }

    /// A table in a centred block's room (#2114): laid out in the room, a
    /// narrower one placed by `w:tblInd` after `tblCellSpacing` and before
    /// `tblBorders`, a full one centred.
    #[test]
    fn a_table_in_a_room_is_laid_out_and_placed_there() {
        let room = Room {
            indent: 1395,
            width: 9000,
            centred: true,
            body: 11790,
            body_share: false,
        };
        let table = |width, jc| {
            layout(
                RawTable {
                    spec: TableSpec {
                        width: Some(width),
                        outer: [Some(line(6)); 4],
                        inside: [Some(line(6)); 2],
                        cellpadding: Some(0),
                        cellspacing: Some(4),
                        jc,
                        room: Some(room),
                        ..TableSpec::default()
                    },
                    rows: vec![vec![text_cell("a", spec(1, 1))]],
                    groups: Vec::new(),
                    cols: Vec::new(),
                },
                room.width,
                &Budget::standard(),
            )
        };
        let t = table(Width::Twips(4500), Some("right"));
        assert_eq!(t.grid, [4500]);
        let pr = t.raw_tblpr.unwrap();
        assert!(
            pr.starts_with(
                "<w:tblPr><w:tblW w:w=\"4500\" w:type=\"dxa\"/>\
                 <w:tblCellSpacing w:w=\"30\" w:type=\"dxa\"/>\
                 <w:tblInd w:w=\"5895\" w:type=\"dxa\"/><w:tblBorders>"
            ),
            "{pr}"
        );
        let t = table(Width::Pct(100.0), None);
        assert_eq!(t.grid, [9000]);
        let pr = t.raw_tblpr.unwrap();
        assert!(
            pr.starts_with(
                "<w:tblPr><w:tblW w:w=\"9000\" w:type=\"dxa\"/><w:jc w:val=\"center\"/>"
            ),
            "{pr}"
        );
        assert!(!pr.contains("tblInd"), "{pr}");
    }

    #[test]
    fn table_and_cell_properties_are_written_in_schema_order() {
        let t = layout(
            RawTable {
                spec: TableSpec {
                    width: Some(Width::Twips(9000)),
                    outer: [Some(line(12)); 4],
                    inside: [Some(line(6)); 2],
                    cellpadding: Some(0),
                    cellspacing: Some(4),
                    jc: Some("center"),
                    room: None,
                    ..TableSpec::default()
                },
                rows: vec![vec![text_cell(
                    "a",
                    CellSpec {
                        colspan: 1,
                        rowspan: 1,
                        width: None,
                        fill: Some("FF0000".into()),
                        valign: VAlign::Top,
                        padding: [Some(150), None, Some(-30), Some(300)],
                        ..CellSpec::default()
                    },
                )]],
                groups: Vec::new(),
                cols: Vec::new(),
            },
            BODY_TWIPS,
            &Budget::standard(),
        );
        let b = "w:val=\"single\" w:sz=\"12\" w:space=\"0\" w:color=\"auto\"/>";
        let i = "w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"auto\"/>";
        let z = "w:w=\"0\" w:type=\"dxa\"/>";
        assert_eq!(
            t.raw_tblpr.as_deref().unwrap(),
            format!(
                "<w:tblPr><w:tblW w:w=\"9000\" w:type=\"dxa\"/><w:jc w:val=\"center\"/>\
                 <w:tblCellSpacing w:w=\"30\" w:type=\"dxa\"/><w:tblBorders><w:top {b}\
                 <w:left {b}<w:bottom {b}<w:right {b}<w:insideH {i}<w:insideV {i}\
                 </w:tblBorders><w:tblCellMar><w:top {z}<w:left {z}<w:bottom {z}\
                 <w:right {z}</w:tblCellMar></w:tblPr>"
            )
        );
        assert_eq!(
            t.rows[0].cells[0].raw_tcpr.as_deref().unwrap(),
            "<w:tcPr><w:tcW w:w=\"9000\" w:type=\"dxa\"/><w:shd w:val=\"clear\" \
             w:color=\"auto\" w:fill=\"FF0000\"/><w:tcMar><w:top w:w=\"150\" w:type=\"dxa\"/>\
             <w:left w:w=\"300\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/></w:tcMar>\
             <w:vAlign w:val=\"top\"/></w:tcPr>"
        );
        // No border, padding or spacing: none written.
        let (_, pr) = widths(None, &[None]);
        assert_eq!(pr, "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>");
    }

    #[test]
    fn a_cell_absorbed_past_max_columns_keeps_its_centred_paragraphs() {
        let boxed = |c: &mut RawCell| {
            c.boxes.push((
                0,
                CellCentring {
                    boxes: vec![CellBox {
                        width: 1000,
                        ..CellBox::default()
                    }],
                    ..CellCentring::default()
                },
            ));
        };
        let mut first = text_cell("a", spec(1, 1));
        boxed(&mut first);
        let mut late = text_cell("b", spec(1, 1));
        boxed(&mut late);
        first.absorb(late);
        assert_eq!(
            first.boxes.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            [0, 1]
        );
        let t = layout(
            RawTable {
                spec: TableSpec {
                    width: Some(Width::Twips(9000)),
                    ..TableSpec::default()
                },
                rows: vec![vec![first]],
                groups: Vec::new(),
                cols: Vec::new(),
            },
            BODY_TWIPS,
            &Budget::standard(),
        );
        // Both paragraphs centred in the 9000 cell: 4000 each side.
        let indents: Vec<(i32, i32)> = t.rows[0].cells[0]
            .blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => (p.props.indent, p.props.indent_right),
                b => panic!("{b:?}"),
            })
            .collect();
        assert_eq!(indents, [(4000, 4000), (4000, 4000)]);
    }

    #[test]
    fn a_line_moves_only_with_the_boxes_its_block_is_in() {
        let mut blocks = vec![Block::Paragraph(Paragraph::default())];
        let Some(Block::Paragraph(p)) = blocks.first_mut() else {
            unreachable!()
        };
        let mut l = line(8);
        l.space = 1;
        p.props.borders.left = Some(l);
        p.props.borders.right = Some(l);
        // Two boxes, 6000 then 2000 wide, in a 9000 cell: 1500, then 2000
        // more. The left line is in the outer box only, the right one in
        // both; each was 40 twips from the text (1 pt after its width).
        let c = CellCentring {
            boxes: vec![
                CellBox {
                    width: 6000,
                    ..CellBox::default()
                },
                CellBox {
                    width: 2000,
                    ..CellBox::default()
                },
            ],
            lines: [Some((1, -40)), Some((2, -40))],
            ..CellCentring::default()
        };
        centre_in_cell(&mut blocks, vec![(0, c)], 9000);
        let Some(Block::Paragraph(p)) = blocks.first() else {
            unreachable!()
        };
        assert_eq!((p.props.indent, p.props.indent_right), (3500, 3500));
        // Left: 3500 + 40 - 1500 = 2040 twips, less 20 for the line, is
        // 101 pt, Word's 31 at most; right: 3500 + 40 - 3500 = 40, 1 pt.
        assert_eq!(p.props.borders.left.map(|b| b.space), Some(31));
        assert_eq!(p.props.borders.right.map(|b| b.space), Some(1));
        assert_eq!(line_space(40, 8), 1);
    }

    #[test]
    fn a_nested_table_takes_its_cell_width() {
        let inner = RawTable {
            spec: TableSpec {
                width: Some(Width::Pct(100.0)),
                ..TableSpec::default()
            },
            rows: vec![vec![text_cell("in", spec(1, 1))]],
            groups: Vec::new(),
            cols: Vec::new(),
        };
        let mut outer = text_cell("out", spec(1, 1));
        outer.nested.push((1, inner));
        outer.blocks.push(placeholder());
        let t = layout(
            RawTable {
                spec: TableSpec {
                    width: Some(Width::Twips(9000)),
                    cellpadding: Some(10),
                    ..TableSpec::default()
                },
                rows: vec![vec![outer, text_cell("b", spec(1, 1))]],
                groups: Vec::new(),
                cols: Vec::new(),
            },
            BODY_TWIPS,
            &Budget::standard(),
        );
        assert_eq!(t.grid, [4500, 4500]);
        let blocks = &t.rows[0].cells[0].blocks;
        let Some(Block::Table(n)) = blocks.get(1) else {
            panic!("{blocks:?}")
        };
        // 4500 less the cell's 150 + 150 padding.
        assert_eq!(n.grid, [4200]);
        // The cell ends in a paragraph after the table.
        assert!(matches!(blocks.last(), Some(Block::Paragraph(_))));
        assert_eq!(blocks.len(), 3);
    }
}
