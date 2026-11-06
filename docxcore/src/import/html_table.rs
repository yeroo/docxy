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

use super::{Budget, MAX_COLUMNS};
use crate::model::{Block, Cell, Paragraph, Row, Table, VMerge};

/// The narrowest column, in twips (one px).
pub(crate) const MIN_COL: u32 = 15;
/// Word's widest page measure, in twips (22"): no width exceeds it.
pub(crate) const MAX_TWIPS: u32 = 31_680;
/// The page an HTML import is laid out for (#1928): US Letter, written
/// explicitly so a reader's locale (A4) never changes the body width.
pub(crate) const PAGE_W_TWIPS: u32 = 12_240;
pub(crate) const PAGE_H_TWIPS: u32 = 15_840;
/// The page's margin on every side (#1928): 15 px, about the inset Outlook's
/// reading pane gives a mail body (a browser's own body margin is 8 px),
/// instead of Word's 1".
pub(crate) const PAGE_MARGIN_TWIPS: u32 = 225;
/// The room an outermost table has: the body of that page, 786 px.
pub(crate) const BODY_TWIPS: u32 = PAGE_W_TWIPS - 2 * PAGE_MARGIN_TWIPS;

/// The final section of an HTML import (#1928): the page and margins
/// [`BODY_TWIPS`] is laid out for. Header and footer sit in the same 15 px
/// band (an imported page has neither); no gutter.
pub(crate) fn page_section() -> crate::model::SectionProperties {
    let m = PAGE_MARGIN_TWIPS;
    crate::model::SectionProperties {
        raw: format!(
            "<w:sectPr><w:pgSz w:w=\"{PAGE_W_TWIPS}\" w:h=\"{PAGE_H_TWIPS}\"/>\
             <w:pgMar w:top=\"{m}\" w:right=\"{m}\" w:bottom=\"{m}\" w:left=\"{m}\" \
             w:header=\"{m}\" w:footer=\"{m}\" w:gutter=\"0\"/></w:sectPr>"
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
    /// `border`, in px (0: none).
    pub border: u32,
    /// `cellpadding` / `cellspacing`, in px.
    pub cellpadding: Option<u32>,
    pub cellspacing: Option<u32>,
    /// `w:jc`, `center` or `right`; `None` is the left default (#1922):
    /// a float `align=right` (`align=left` is none), else the table's auto
    /// side margins (`<table align=center>` is that hint), else its
    /// container's `-webkit-` alignment.
    pub jc: Option<&'static str>,
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
        }
    }
}

/// A cell as read: its blocks, with a placeholder at each index a nested
/// table holds (`nested`), laid out with the table.
#[derive(Debug, Default)]
pub(crate) struct RawCell {
    pub spec: CellSpec,
    pub blocks: Vec<Block>,
    pub nested: Vec<(usize, RawTable)>,
}

impl RawCell {
    /// Take `other`'s blocks and nested tables after this cell's own (a cell
    /// with no room in its row).
    pub(crate) fn absorb(&mut self, other: RawCell) {
        let base = self.blocks.len();
        self.blocks.extend(other.blocks);
        self.nested
            .extend(other.nested.into_iter().map(|(i, t)| (base + i, t)));
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
pub(crate) fn layout(t: RawTable, avail: u32, budget: &Budget) -> Table {
    let avail = avail.clamp(MIN_COL, MAX_TWIPS);
    let RawTable { spec, rows, groups } = t;
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
    let grid = column_widths(&spec, &placed, ncols, avail);
    let span_w = |col: usize, span: usize| -> u32 { grid[col..col + span].iter().sum() };
    let pad = spec.cellpadding.map(px_twips);

    // Each placed cell's tcPr (the continuation cells under it share it,
    // vMerge aside) and its blocks, nested tables laid out in its width.
    let mut tcprs = Vec::with_capacity(placed.len());
    let mut cells: Vec<Option<Vec<Block>>> = Vec::with_capacity(placed.len());
    let reach: Vec<usize> = placed.iter().map(|p| p.rows).collect();
    for p in placed.iter() {
        let w = span_w(p.col, p.span);
        tcprs.push(TcPr {
            width: w,
            span: p.span as u32,
            fill: p.cell.spec.fill.clone(),
            valign: p.cell.spec.valign,
            padding: p.cell.spec.padding,
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
        let RawCell {
            mut blocks, nested, ..
        } = p.cell;
        for (at, raw) in nested {
            let table = layout(raw, inner, budget);
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
    for row_slots in &slots {
        let mut row = Row::default();
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
                    let blocks = cells[i].take().unwrap_or_default();
                    (make_cell(p, merge, blocks), p.span as usize)
                }
                Slot::Covered(i) => {
                    let p = &tcprs[i];
                    let blocks = vec![Block::Paragraph(Paragraph::default())];
                    (make_cell(p, VMerge::Continue, blocks), p.span as usize)
                }
                Slot::Free => {
                    let p = TcPr {
                        width: grid[c],
                        span: 1,
                        fill: None,
                        valign: VAlign::Center,
                        padding: [None; 4],
                    };
                    let blocks = vec![Block::Paragraph(Paragraph::default())];
                    (make_cell(&p, VMerge::None, blocks), 1)
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

    Table {
        grid,
        rows: out_rows,
        raw_tblpr: Some(tblpr(&spec)),
        ..Table::default()
    }
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
/// least [`MIN_COL`]. A table's width is its own, else the columns' when
/// every one has a width, else `avail`; declared columns narrower than a
/// declared table grow to it, and columns wider than the table widen it.
fn column_widths(spec: &TableSpec, placed: &[Placed], ncols: usize, avail: u32) -> Vec<u32> {
    let declared = spec.width.map(|w| resolve(w, avail));
    let reference = declared.unwrap_or(avail);
    let single: Vec<&Placed> = placed.iter().filter(|p| p.span == 1).collect();
    // Percent columns, scaled to 100 in all.
    let mut pct = vec![0.0f64; ncols];
    for p in &single {
        if let Some(Width::Pct(v)) = p.cell.spec.width {
            pct[p.col] = pct[p.col].max(v.max(0.0));
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
    for p in &single {
        if let Some(Width::Twips(t)) = p.cell.spec.width {
            col[p.col] = Some(col[p.col].unwrap_or(0).max(t));
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
}

fn make_cell(p: &TcPr, merge: VMerge, blocks: Vec<Block>) -> Cell {
    Cell {
        grid_span: p.span.max(1),
        v_merge: merge,
        blocks,
        raw_tcpr: Some(tcpr(p, merge)),
        ..Cell::default()
    }
}

/// `w:tcPr`, its children in schema order: tcW, gridSpan, vMerge, shd,
/// tcMar, vAlign.
fn tcpr(p: &TcPr, merge: VMerge) -> String {
    let mut s = format!("<w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>", p.width);
    if p.span > 1 {
        s.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", p.span));
    }
    match merge {
        VMerge::None => {}
        VMerge::Restart => s.push_str("<w:vMerge w:val=\"restart\"/>"),
        VMerge::Continue => s.push_str("<w:vMerge/>"),
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
/// tblBorders, tblCellMar.
fn tblpr(spec: &TableSpec) -> String {
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
    if let Some(n) = spec.cellspacing.filter(|&n| n > 0) {
        // Word's spacing is on each side of a cell: half the HTML gap
        // (unmeasured against Word).
        s.push_str(&format!(
            "<w:tblCellSpacing w:w=\"{}\" w:type=\"dxa\"/>",
            px_twips(n) / 2
        ));
    }
    if spec.border > 0 {
        let sz = (u64::from(spec.border) * 6).clamp(2, 96);
        s.push_str("<w:tblBorders>");
        for edge in ["w:top", "w:left", "w:bottom", "w:right"] {
            s.push_str(&format!(
                "<{edge} w:val=\"single\" w:sz=\"{sz}\" w:space=\"0\" w:color=\"auto\"/>"
            ));
        }
        // A browser gives the cells of a bordered table 1 px borders.
        for edge in ["w:insideH", "w:insideV"] {
            s.push_str(&format!(
                "<{edge} w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"auto\"/>"
            ));
        }
        s.push_str("</w:tblBorders>");
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

    #[test]
    fn table_and_cell_properties_are_written_in_schema_order() {
        let t = layout(
            RawTable {
                spec: TableSpec {
                    width: Some(Width::Twips(9000)),
                    border: 2,
                    cellpadding: Some(0),
                    cellspacing: Some(4),
                    jc: Some("center"),
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
                    },
                )]],
                groups: Vec::new(),
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
    fn a_nested_table_takes_its_cell_width() {
        let inner = RawTable {
            spec: TableSpec {
                width: Some(Width::Pct(100.0)),
                ..TableSpec::default()
            },
            rows: vec![vec![text_cell("in", spec(1, 1))]],
            groups: Vec::new(),
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
