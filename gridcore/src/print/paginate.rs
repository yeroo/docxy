//! The pages a print job prints on, laid out as Excel lays them out.
//!
//! A sheet prints its print area (each range on pages of its own, in stored
//! order) or, without one, its used area from A1. Hidden rows and columns
//! are skipped. Pages break where a manual break says, and otherwise where
//! the next row or column would cross the printable area: the paper less
//! its margins, at the sheet's scale (`Adjust to`, or the largest whole
//! percentage that fits `Fit to` pages, never above 100 %). While `Fit to`
//! is on, manual breaks are ignored, as Excel ignores them. Print titles
//! repeat on every page that doesn't already show them, except titles that
//! would fill a page by themselves, which don't repeat at all (our rule). Pages run down then
//! over, or over then down, and are numbered across the whole job.
//!
//! Sizes: a column `width` w (in character units, padding included) is
//! `trunc(((256·w + trunc(128/7))/256)·7)` pixels (ECMA-376 §18.3.1.13, with
//! Calibri 11's maximum digit width of 7 px); a sheet without
//! `defaultColWidth` sizes its columns at `baseColWidth`·7 + 5 px rounded up
//! to a multiple of 8 (64 px for the familiar 8.43). A row is its `ht`, or
//! the sheet's `defaultRowHeight`, or 15 pt. A pixel is 0.75 pt.

use std::sync::Arc;

use super::area::{Rect, print_area, print_titles};
use super::setup::PageOrder;
use crate::sheet::{MAX_COLS, MAX_ROWS, Sheet, Workbook};

/// What to print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// The active sheets: the grouped sheets, each starting a new page.
    ActiveSheets(Vec<usize>),
    /// Every visible sheet.
    EntireWorkbook,
    /// These ranges of one sheet, each on pages of its own.
    Selection { sheet: usize, ranges: Vec<Rect> },
}

/// A print job: what, and the Settings that change the pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub what: What,
    /// `Ignore Print Area`: print the used area even where a print area is
    /// set.
    pub ignore_print_areas: bool,
    /// `Pages from`, 1-based position in the job; `None` is the first page.
    pub from: Option<u32>,
    /// `Pages to`; `None` is the last page.
    pub to: Option<u32>,
}

impl Job {
    pub fn new(what: What) -> Job {
        Job {
            what,
            ignore_print_areas: false,
            from: None,
            to: None,
        }
    }
}

/// One printed page.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub sheet: usize,
    /// The sheet rows printed in the body, in order (hidden rows left out).
    pub rows: Lines,
    /// The sheet columns printed in the body, in order.
    pub cols: Lines,
    /// Title rows repeated above the body on this page (none where the body
    /// already shows them).
    pub title_rows: Lines,
    /// Title columns repeated left of the body.
    pub title_cols: Lines,
    /// The page number printed for `&P`.
    pub number: u32,
    /// The page's position within its sheet's pages, 1-based: what
    /// `Different first page` and odd/even headers go by.
    pub sheet_page: u32,
    /// The scale the page prints at (1.0 = 100 %).
    pub scale: f64,
}

/// A run of sheet rows or columns on a page: a span of one list shared by
/// every page of its range (or, for titles, its sheet), so a job's storage
/// is its lines once, not once a page. Reads as a `&[u32]`.
#[derive(Clone, Debug)]
pub struct Lines {
    all: Arc<[u32]>,
    start: usize,
    end: usize,
}

impl Lines {
    /// `all[start..end]`, sharing `all`.
    fn span(all: &Arc<[u32]>, start: usize, end: usize) -> Lines {
        Lines {
            all: Arc::clone(all),
            start,
            end,
        }
    }

    /// The whole of `all`.
    fn whole(all: &Arc<[u32]>) -> Lines {
        Lines::span(all, 0, all.len())
    }

    /// Do `self` and `other` share one list?
    pub fn shares(&self, other: &Lines) -> bool {
        Arc::ptr_eq(&self.all, &other.all)
    }
}

impl std::ops::Deref for Lines {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        &self.all[self.start..self.end]
    }
}

impl<'a> IntoIterator for &'a Lines {
    type Item = &'a u32;
    type IntoIter = std::slice::Iter<'a, u32>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl PartialEq for Lines {
    fn eq(&self, other: &Lines) -> bool {
        **self == **other
    }
}

impl PartialEq<Vec<u32>> for Lines {
    fn eq(&self, other: &Vec<u32>) -> bool {
        **self == **other
    }
}

impl Page {
    /// The body's (first row, first col, last row, last col).
    pub fn range(&self) -> Rect {
        (
            self.rows.first().copied().unwrap_or(0),
            self.cols.first().copied().unwrap_or(0),
            self.rows.last().copied().unwrap_or(0),
            self.cols.last().copied().unwrap_or(0),
        )
    }
}

/// The pages of a job.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pages {
    /// The pages printed (after the from/to filter).
    pub pages: Vec<Page>,
    /// How many pages the whole job has: `&N`.
    pub total: u32,
    /// The job has more than [`MAX_PAGES`] pages: layout stopped before the
    /// range that would cross the limit, and laid out no sheet after it.
    pub truncated: bool,
}

/// The most pages one job lays out. A used area that runs to the sheet's
/// last row prints a few thousand pages even at 10 %; past this, the job is
/// cut short and [`Pages::truncated`] says so.
pub const MAX_PAGES: usize = 100_000;

/// Maximum digit width of the default font (Calibri 11) in pixels.
const MDW: f64 = 7.0;
/// Points per pixel at 96 dpi.
const PT_PER_PX: f64 = 0.75;
/// Excel's row height when the sheet names none.
const DEFAULT_ROW_PT: f64 = 15.0;

/// Pixels of a stored column width (ECMA-376 §18.3.1.13).
pub fn width_px(w: f64) -> f64 {
    ((256.0 * w + (128.0 / MDW).trunc()) / 256.0 * MDW).trunc()
}

/// Pixels of a column with no width of its own.
fn default_col_px(sheet: &Sheet) -> f64 {
    match sheet.format.default_col_width {
        Some(w) => width_px(w),
        None => {
            let px = f64::from(sheet.format.base_col_width) * MDW + 5.0;
            (px / 8.0).ceil() * 8.0
        }
    }
}

/// A column's printed width in points at 100 %; 0 for a hidden column.
pub fn col_points(sheet: &Sheet, col: u32) -> f64 {
    for d in &sheet.col_defs {
        if col >= d.min && col <= d.max {
            if sheet.col_hidden(col) {
                return 0.0;
            }
            return match d.width {
                Some(w) => width_px(w),
                None => default_col_px(sheet),
            } * PT_PER_PX;
        }
    }
    default_col_px(sheet) * PT_PER_PX
}

/// A row's printed height in points at 100 %; 0 for a hidden row.
pub fn row_points(sheet: &Sheet, row: u32) -> f64 {
    if sheet.row_hidden(row) {
        return 0.0;
    }
    sheet
        .row_height(row)
        .or(sheet.format.default_row_height)
        .unwrap_or(DEFAULT_ROW_PT)
}

/// The printable body (width, height) in points: the paper, less margins
/// and, when headings print, the heading strips.
pub fn body_points(wb: &Workbook, sheet: usize) -> (f64, f64) {
    let s = &wb.sheets[sheet];
    let ps = &s.page_setup;
    let (w, h) = ps.page_points();
    let m = &ps.margins;
    let mut bw = w - (m.left + m.right) * 72.0;
    let mut bh = h - (m.top + m.bottom) * 72.0;
    if ps.headings {
        let (hw, hh) = heading_points(s);
        bw -= hw;
        bh -= hh;
    }
    (bw.max(1.0), bh.max(1.0))
}

/// The row-heading strip's width and the column-heading strip's height, in
/// points at 100 %.
pub fn heading_points(sheet: &Sheet) -> (f64, f64) {
    let (rows, _) = sheet.used_size();
    let digits = rows.max(1).to_string().len() as f64;
    ((digits * MDW + 10.0) * PT_PER_PX, DEFAULT_ROW_PT)
}

/// Is a cell printed at all: a value, or a fill or border that shows?
fn prints(wb: &Workbook, sheet: &Sheet, row: u32, col: u32) -> bool {
    let Some(cell) = sheet.cell(row, col) else {
        return false;
    };
    if !cell.value.is_empty() {
        return true;
    }
    if cell.style == 0 {
        return false;
    }
    let xf = wb.styles.xf(cell.style);
    xf.fill.is_some() || xf.border
}

/// The used area: A1 to the last row and column with a printed cell.
pub fn used_area(wb: &Workbook, sheet: usize) -> Option<Rect> {
    printed_extent(wb, sheet, (0, 0, MAX_ROWS - 1, MAX_COLS - 1))
}

/// The ranges a sheet prints, in order.
fn sheet_ranges(wb: &Workbook, sheet: usize, ignore_print_areas: bool) -> Vec<Rect> {
    let used = used_area(wb, sheet);
    let area = if ignore_print_areas {
        Vec::new()
    } else {
        print_area(wb, sheet)
    };
    if area.is_empty() {
        return used.into_iter().collect();
    }
    // A whole-row or whole-column area prints only as far as the used area.
    let (ur, uc) = used.map_or((0, 0), |(_, _, r, c)| (r, c));
    area.into_iter()
        .map(|(r1, c1, r2, c2)| {
            let r2 = if r2 >= MAX_ROWS - 1 { ur.max(r1) } else { r2 };
            let c2 = if c2 >= MAX_COLS - 1 { uc.max(c1) } else { c2 };
            (r1, c1, r2, c2)
        })
        .collect()
}

/// One axis of a range: the visible lines and their sizes at 100 %.
#[derive(Clone)]
struct Axis {
    lines: Vec<u32>,
    sizes: Vec<f64>,
}

fn axis(sheet: &Sheet, from: u32, to: u32, rows: bool) -> Axis {
    let mut lines = Vec::new();
    let mut sizes = Vec::new();
    for i in from..=to {
        let size = if rows {
            row_points(sheet, i)
        } else {
            col_points(sheet, i)
        };
        if size > 0.0 {
            lines.push(i);
            sizes.push(size);
        }
    }
    Axis { lines, sizes }
}

/// The titles (visible lines and total size at 100 %) for an axis.
fn titles(sheet: &Sheet, span: Option<(u32, u32)>, rows: bool) -> Axis {
    match span {
        Some((a, b)) => axis(sheet, a, b, rows),
        None => Axis {
            lines: Vec::new(),
            sizes: Vec::new(),
        },
    }
}

/// The titles of one axis as they repeat at `scale` in `room`: none when
/// they would fill the whole page by themselves, so the body paginates as if
/// there were no titles rather than one line a page. (Our rule: the spec
/// doesn't say what Excel does with titles taller than a page.)
fn repeatable(title: Axis, room: f64, scale: f64) -> Axis {
    if title.sizes.iter().sum::<f64>() * scale >= room {
        Axis {
            lines: Vec::new(),
            sizes: Vec::new(),
        }
    } else {
        title
    }
}

/// Split an axis into bands (index ranges into `ax.lines`) that fit `room`
/// at `scale`. A band starts at a manual break or where the next line would
/// overflow; a band that begins past the titles repeats them, so its room is
/// less their size. Every band holds at least one line.
fn bands(ax: &Axis, title: &Axis, room: f64, scale: f64, breaks: &[u32]) -> Vec<(usize, usize)> {
    let title_end = title.lines.last().copied();
    let title_size: f64 = title.sizes.iter().sum::<f64>() * scale;
    let mut out = Vec::new();
    let mut i = 0;
    while i < ax.lines.len() {
        let start = i;
        let repeats = title_end.is_some_and(|t| ax.lines[start] > t);
        let avail = room - if repeats { title_size } else { 0.0 };
        let mut used = 0.0;
        while i < ax.lines.len() {
            let size = ax.sizes[i] * scale;
            let manual = i > start && breaks.contains(&ax.lines[i]);
            if manual || (i > start && used + size > avail + 1e-6) {
                break;
            }
            used += size;
            i += 1;
        }
        out.push((start, i));
    }
    out
}

/// The manual break ids of one axis.
fn manual(breaks: &[crate::sheet::PageBreak]) -> Vec<u32> {
    breaks
        .iter()
        .filter(|b| b.is_manual())
        .map(|b| b.id)
        .collect()
}

/// The manual breaks a sheet's pages obey: (rows, cols). None while `Fit to`
/// is on, which ignores them, as Excel does.
fn obeyed_breaks(s: &Sheet) -> (Vec<u32>, Vec<u32>) {
    if s.page_setup.fit_to_page {
        (Vec::new(), Vec::new())
    } else {
        (manual(&s.row_breaks), manual(&s.col_breaks))
    }
}

/// `rect` cut down to its printed cells: from its top-left corner to the
/// last row and column inside it that hold a printed cell. `None` when it
/// holds none, so an empty selection prints nothing.
fn printed_extent(wb: &Workbook, sheet: usize, (r1, c1, r2, c2): Rect) -> Option<Rect> {
    let s = &wb.sheets[sheet];
    let mut last: Option<(u32, u32)> = None;
    for &(r, c) in s.cells.keys() {
        if (r1..=r2).contains(&r) && (c1..=c2).contains(&c) && prints(wb, s, r, c) {
            let (lr, lc) = last.unwrap_or((r1, c1));
            last = Some((lr.max(r), lc.max(c)));
        }
    }
    last.map(|(r, c)| (r1, c1, r, c))
}

/// The scale a sheet prints at: `Adjust to`, or with `Fit to` the largest
/// whole percentage (10–100) at which every range fits.
fn sheet_scale(wb: &Workbook, sheet: usize, ranges: &[Rect]) -> f64 {
    let s = &wb.sheets[sheet];
    let ps = &s.page_setup;
    if !ps.fit_to_page {
        return f64::from(ps.scale.clamp(10, 400)) / 100.0;
    }
    if ps.fit_width == 0 && ps.fit_height == 0 {
        return 1.0;
    }
    let (room_w, room_h) = body_points(wb, sheet);
    let t = print_titles(wb, sheet);
    let (tr, tc) = (titles(s, t.rows, true), titles(s, t.cols, false));
    let (rb, cb) = obeyed_breaks(s);
    let axes: Vec<(Axis, Axis)> = ranges
        .iter()
        .map(|&(r1, c1, r2, c2)| (axis(s, r1, r2, true), axis(s, c1, c2, false)))
        .collect();
    let none = Axis {
        lines: Vec::new(),
        sizes: Vec::new(),
    };
    // Whether each axis's titles repeat at a scale: (rows, cols).
    let regime = |pct: u32| {
        let k = f64::from(pct) / 100.0;
        (
            !repeatable(tr.clone(), room_h, k).lines.is_empty(),
            !repeatable(tc.clone(), room_w, k).lines.is_empty(),
        )
    };
    let fits = |pct: u32, (keep_r, keep_c): (bool, bool)| {
        let k = f64::from(pct) / 100.0;
        let (tr, tc) = (
            if keep_r { &tr } else { &none },
            if keep_c { &tc } else { &none },
        );
        axes.iter().all(|(rows, cols)| {
            (ps.fit_width == 0 || bands(cols, tc, room_w, k, &cb).len() <= ps.fit_width as usize)
                && (ps.fit_height == 0
                    || bands(rows, tr, room_h, k, &rb).len() <= ps.fit_height as usize)
        })
    };
    // Whether titles repeat changes with the scale ([`repeatable`]), and a
    // scale just under that threshold leaves the body less room than one
    // just over it, so fitting isn't monotonic across the whole 10–100.
    // Within a run of scales where neither axis changes regime it is: fewer
    // pages as the scale falls. So: the largest fitting percentage of each
    // run, and the largest of those.
    let mut best = None;
    let mut hi = 100u32;
    while hi >= 10 {
        let r = regime(hi);
        let mut lo = hi;
        while lo > 10 && regime(lo - 1) == r {
            lo -= 1;
        }
        // The largest fitting percentage in lo..=hi, if any.
        if fits(lo, r) {
            let (mut a, mut b) = (lo, hi);
            while a < b {
                let mid = (a + b).div_ceil(2);
                if fits(mid, r) {
                    a = mid;
                } else {
                    b = mid - 1;
                }
            }
            best = best.max(Some(a));
        }
        if lo == 10 {
            break;
        }
        hi = lo - 1;
    }
    f64::from(best.unwrap_or(10)) / 100.0
}

/// The pages of one sheet's ranges, numbered from 1 within the sheet, at
/// most `budget` of them; `true` when more were cut off.
fn sheet_pages(wb: &Workbook, sheet: usize, ranges: &[Rect], budget: usize) -> (Vec<Page>, bool) {
    let s = &wb.sheets[sheet];
    let ps = &s.page_setup;
    let scale = sheet_scale(wb, sheet, ranges);
    let (room_w, room_h) = body_points(wb, sheet);
    let t = print_titles(wb, sheet);
    let tr = repeatable(titles(s, t.rows, true), room_h, scale);
    let tc = repeatable(titles(s, t.cols, false), room_w, scale);
    let (shared_rows, shared_cols): (Arc<[u32]>, Arc<[u32]>) =
        (tr.lines.as_slice().into(), tc.lines.as_slice().into());
    let none: Arc<[u32]> = Arc::from([]);
    let (title_rows_all, title_cols_all, no_titles) = (
        Lines::whole(&shared_rows),
        Lines::whole(&shared_cols),
        Lines::whole(&none),
    );
    let (rb, cb) = obeyed_breaks(s);
    let mut out = Vec::new();
    for &(r1, c1, r2, c2) in ranges {
        let rows = axis(s, r1, r2, true);
        let cols = axis(s, c1, c2, false);
        if rows.lines.is_empty() || cols.lines.is_empty() {
            continue;
        }
        let row_bands = bands(&rows, &tr, room_h, scale, &rb);
        let col_bands = bands(&cols, &tc, room_w, scale, &cb);
        if out.len() + row_bands.len().saturating_mul(col_bands.len()) > budget {
            return (out, true);
        }
        let mut grid = Vec::new();
        match ps.page_order {
            PageOrder::DownThenOver => {
                for cband in &col_bands {
                    for rband in &row_bands {
                        grid.push((*rband, *cband));
                    }
                }
            }
            PageOrder::OverThenDown => {
                for rband in &row_bands {
                    for cband in &col_bands {
                        grid.push((*rband, *cband));
                    }
                }
            }
        }
        // One shared list per axis of the range; each page holds a span.
        let (row_all, col_all): (Arc<[u32]>, Arc<[u32]>) =
            (rows.lines.as_slice().into(), cols.lines.as_slice().into());
        for ((ra, rz), (ca, cz)) in grid {
            let body_rows = Lines::span(&row_all, ra, rz);
            let body_cols = Lines::span(&col_all, ca, cz);
            let title_rows = match tr.lines.last() {
                Some(&end) if body_rows[0] > end => title_rows_all.clone(),
                _ => no_titles.clone(),
            };
            let title_cols = match tc.lines.last() {
                Some(&end) if body_cols[0] > end => title_cols_all.clone(),
                _ => no_titles.clone(),
            };
            out.push(Page {
                sheet,
                rows: body_rows,
                cols: body_cols,
                title_rows,
                title_cols,
                number: 0,
                sheet_page: out.len() as u32 + 1,
                scale,
            });
        }
    }
    (out, false)
}

/// Lay out a print job. No pages means there is nothing to print.
pub fn paginate(wb: &Workbook, job: &Job) -> Pages {
    let jobs: Vec<(usize, Vec<Rect>)> = match &job.what {
        // Each sheet once. A sheet named here prints even when hidden (the
        // editor shows and edits hidden sheets); only the entire workbook
        // leaves hidden sheets out.
        What::ActiveSheets(list) => {
            let mut seen = std::collections::HashSet::new();
            list.iter()
                .filter(|&&i| i < wb.sheets.len() && seen.insert(i))
                .map(|&i| (i, sheet_ranges(wb, i, job.ignore_print_areas)))
                .collect()
        }
        What::EntireWorkbook => (0..wb.sheets.len())
            .filter(|&i| !wb.sheets[i].hidden)
            .map(|i| (i, sheet_ranges(wb, i, job.ignore_print_areas)))
            .collect(),
        // Only as far as the printed cells: `A:A` prints the used rows, and
        // a range with nothing in it prints nothing.
        What::Selection { sheet, ranges } if *sheet < wb.sheets.len() => vec![(
            *sheet,
            ranges
                .iter()
                .filter_map(|&r| printed_extent(wb, *sheet, r))
                .collect(),
        )],
        What::Selection { .. } => Vec::new(),
    };
    let mut all = Vec::new();
    let mut next = 1u32;
    let mut truncated = false;
    for (sheet, ranges) in jobs {
        let (mut pages, cut) = sheet_pages(wb, sheet, &ranges, MAX_PAGES - all.len());
        if let Some(n) = wb.sheets[sheet]
            .page_setup
            .first_page_number
            .filter(|_| !pages.is_empty())
        {
            next = n;
        }
        for p in &mut pages {
            p.number = next;
            next = next.saturating_add(1);
        }
        all.extend(pages);
        if cut {
            truncated = true;
            break;
        }
    }
    let total = all.len() as u32;
    let from = job.from.unwrap_or(1).max(1);
    let to = job.to.unwrap_or(total);
    let pages = all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| {
            let n = *i as u32 + 1;
            n >= from && n <= to
        })
        .map(|(_, p)| p)
        .collect();
    Pages {
        pages,
        total,
        truncated,
    }
}

#[cfg(test)]
mod tests;
