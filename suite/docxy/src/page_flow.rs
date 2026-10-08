//! Print Layout pagination for the suite's page view (#745): flow a shown
//! body into pages/bands/columns with each section's own geometry, agreeing
//! with the PDF exporter (`docxcore/src/export.rs`), which is the oracle for
//! every rule here (section splitting, geometry, `w:type` starts, balancing).

use docxcore::model::Block;
use docxcore::sect::{PageNumberFormat, SectionSetup, SectionStart};

/// Word's limit on newspaper columns in a section (export.rs `MAX_COLS`).
const MAX_COLS: i32 = 45;

/// One section as the page view draws it, all twips. `left` includes the
/// gutter (when it sits at the side; else `top` does), like export.rs
/// `h_margins`/`top_margin` without mirror margins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionBox {
    pub start: SectionStart,
    pub w: i32,
    pub h: i32,
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
    /// One width per column (equal widths, or each `w:col`'s `w:w`).
    pub col_w: Vec<i32>,
    /// The space after each column (the last entry is unused), the second
    /// half of export.rs's `cols: Vec<(w, space)>`.
    pub col_gap: Vec<i32>,
    pub sep: bool,
    pub num_start: Option<i32>,
}

/// One section's columns on a page: the block range each column holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Band {
    pub section: usize,
    pub cols: Vec<(usize, usize)>,
}

/// A printed sheet. `bands[0]`'s section owns the sheet: its page size and
/// top/bottom margins; every band carries its own left/right margins and
/// columns. A page always has at least one band, and a band at least one
/// (possibly empty) column range. `filler` marks the blank sheet an
/// oddPage/evenPage start inserts: it belongs to the previous section and
/// must not take its First header/footer variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Page {
    pub bands: Vec<Band>,
    pub filler: bool,
}

/// The sections of a body and the pages they flow into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageFlow {
    pub sections: Vec<SectionBox>,
    pub pages: Vec<Page>,
}

impl PageFlow {
    /// Per page, all bands' column ranges concatenated — the flat per-page
    /// column lists the probes, `page_of_block` and the status bar use.
    pub(crate) fn ranges(&self) -> Vec<Vec<(usize, usize)>> {
        self.pages
            .iter()
            .map(|p| {
                p.bands
                    .iter()
                    .flat_map(|b| b.cols.iter().copied())
                    .collect()
            })
            .collect()
    }

    /// Whether each page is a blank filler sheet.
    pub(crate) fn fillers(&self) -> Vec<bool> {
        self.pages.iter().map(|p| p.filler).collect()
    }

    /// The section and drag geometry of page `tracked`: the sheet size from
    /// the page's owning section, the margins raw from `margins_sect` (the
    /// final section's sectPr — the ruler's margin handles edit it), so a
    /// gutter is not double-counted and a negative margin is not absolutized
    /// by a drag.
    pub(crate) fn drag_geom(
        &self,
        tracked: Option<usize>,
        margins_sect: &str,
    ) -> Option<(usize, docxcore::model::PageGeom)> {
        let section = self.pages.get(tracked?)?.bands.first()?.section;
        let sb = self.sections.get(section)?;
        let mut geom = docxcore::model::PageGeom::from_sect_pr(margins_sect);
        geom.w = sb.w;
        geom.h = sb.h;
        Some((section, geom))
    }
}

/// The first body block of each page in `ranges`: an empty range (the blank
/// page an odd/even section start inserts) takes the previous block, so the
/// filler shows the previous section's header/footer, like the PDF exporter
/// (export.rs `new_page(i - 1, false, true)`).
pub(crate) fn first_blocks(ranges: &[Vec<(usize, usize)>]) -> Vec<usize> {
    ranges
        .iter()
        .map(|cols| {
            cols.first()
                .map_or(0, |&(s, e)| if s == e { s.saturating_sub(1) } else { s })
        })
        .collect()
}

/// A section's drawing geometry from its sectPr, mirroring export.rs
/// `SectionLayout::parse` (margin absolutes, gutter placement, column math,
/// `w:type`) in twips rather than points.
fn section_box(sect: &str, gutter_at_top: bool) -> SectionBox {
    let setup = SectionSetup::parse(sect);
    let m = setup.margins;
    let left = m.left.abs() + if gutter_at_top { 0 } else { m.gutter.abs() };
    let top = m.top.abs() + if gutter_at_top { m.gutter.abs() } else { 0 };
    let cols = &setup.columns;
    let n = cols.count().clamp(1, MAX_COLS) as usize;
    let (col_w, col_gap) = if cols.equal_width() {
        // `left` already holds the side gutter, matching export.rs's
        // `content_w = w - left - right - gutter`.
        let space = cols.space.max(0);
        let text_w = setup.page.w - left - m.right.abs();
        let w = ((text_w - (n as i32 - 1) * space) / n as i32).max(1);
        ((0..n).map(|_| w).collect(), (0..n).map(|_| space).collect())
    } else {
        (
            cols.cols
                .iter()
                .take(MAX_COLS as usize)
                .map(|c| c.w)
                .collect(),
            cols.cols
                .iter()
                .take(MAX_COLS as usize)
                .map(|c| c.space)
                .collect(),
        )
    };
    SectionBox {
        start: setup.start,
        w: setup.page.w,
        h: setup.page.h,
        top,
        bottom: m.bottom.abs(),
        left,
        right: m.right.abs(),
        col_w,
        col_gap,
        sep: cols.sep,
        num_start: PageNumberFormat::parse(sect).start,
    }
}

/// Split a body into sections exactly like `split_sections` in
/// `docxcore/src/export.rs`: a paragraph whose `props.section_break` is
/// `Some(raw)` closes a section whose sectPr is `raw`; the rest, to
/// `body.len()`, is the final section with `last_sect` (authoritative even
/// when the body ends in a `Block::SectionProperties`, which stays a
/// zero-height block inside the final range).
fn split<'a>(body: &'a [Block], last_sect: &'a str) -> Vec<(std::ops::Range<usize>, &'a str)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, b) in body.iter().enumerate() {
        if let Block::Paragraph(p) = b
            && let Some(sect) = &p.props.section_break
        {
            out.push((start..i + 1, sect.as_str()));
            start = i + 1;
        }
    }
    out.push((start..body.len(), last_sect));
    out
}

/// A band while its columns are being filled.
struct BandB {
    section: usize,
    /// Closed column ranges.
    cols: Vec<(usize, usize)>,
    /// The open column's first block.
    col_start: usize,
    /// The next block index to place.
    pos: usize,
    /// Height used in the open column (px).
    acc: f32,
    /// Tallest closed column (px).
    tallest: f32,
}

impl BandB {
    fn new(section: usize, pos: usize) -> Self {
        BandB {
            section,
            cols: Vec::new(),
            col_start: pos,
            pos,
            acc: 0.0,
            tallest: 0.0,
        }
    }

    /// Close the open column at `end` (an empty column closes to nothing).
    fn close_column(&mut self, end: usize) {
        if self.col_start < end {
            self.cols.push((self.col_start, end));
            self.tallest = self.tallest.max(self.acc);
        }
        self.col_start = end;
        self.acc = 0.0;
    }
}

/// A page while its bands are being filled.
struct PageB {
    /// Closed bands; the first owns the sheet.
    bands: Vec<Band>,
    /// The band being filled.
    open: BandB,
    /// Sum of the closed bands' heights (px).
    used_h: f32,
}

impl PageB {
    fn new(section: usize, pos: usize) -> Self {
        PageB {
            bands: Vec::new(),
            open: BandB::new(section, pos),
            used_h: 0.0,
        }
    }

    /// The section that owns the sheet: its size and top/bottom margins.
    fn owner(&self) -> usize {
        self.bands.first().map_or(self.open.section, |b| b.section)
    }
}

/// Balance a closing band's blocks into its columns at the smallest column
/// height the pour fits at, as export.rs `balance_region` does: a greedy
/// `pour(h)` that fails once a block would open a column past `ncols`, and a
/// bisection for the smallest fitting `h`. `heights` are the band's block
/// heights in order; `span` is their block range. `None` when the band is
/// empty or even the full-height pour cannot fit it (the caller keeps the
/// layout as filled). The caller checks the columns are at least two and
/// equal-width.
fn balance_band(
    heights: &[f32],
    (s, e): (usize, usize),
    ncols: usize,
) -> Option<(Vec<(usize, usize)>, f32)> {
    if heights.is_empty() || ncols < 2 {
        return None;
    }
    let pour = |h: f32| -> Option<Vec<(usize, usize)>> {
        let (mut s0, mut acc) = (s, 0.0_f32);
        let mut cols: Vec<(usize, usize)> = Vec::new();
        for (k, &bh) in heights.iter().enumerate() {
            if acc > 0.0 && acc + bh > h {
                // The block opens a new column; it must not pass the last.
                if cols.len() + 1 >= ncols {
                    return None;
                }
                cols.push((s0, s + k));
                s0 = s + k;
                acc = 0.0;
            }
            acc += bh;
        }
        cols.push((s0, e));
        Some(cols)
    };
    let total = heights.iter().sum::<f32>();
    pour(total)?;
    let (mut lo, mut hi) = (heights.iter().copied().fold(0.0_f32, f32::max), total);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if pour(mid).is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let cols = pour(hi)?;
    let height = cols
        .iter()
        .map(|&(a, z)| (a..z).map(|i| heights[i - s]).sum::<f32>())
        .fold(0.0_f32, f32::max);
    Some((cols, height))
}

fn all_equal(ws: &[i32]) -> bool {
    ws.windows(2).all(|w| w[0] == w[1])
}

/// The flow state: sections, finished pages, the page under construction and
/// the running page number (`w:pgNumType w:start` restarts included).
struct Flow<'a> {
    body: &'a [Block],
    sections: Vec<SectionBox>,
    pages: Vec<Page>,
    next_number: i32,
    cur: Option<PageB>,
}

impl<'a> Flow<'a> {
    /// Open a fresh page (a section's first, or a continuation when
    /// `!first`), consuming a page number like export.rs `new_page`.
    fn open_page(&mut self, section: usize, pos: usize, first: bool) {
        let number = if first {
            self.sections[section].num_start.unwrap_or(self.next_number)
        } else {
            self.next_number
        };
        self.next_number = number.saturating_add(1);
        self.cur = Some(PageB::new(section, pos));
    }

    /// Close the open band into the page's band list; `balanced` when a
    /// continuous/nextColumn section ends it (export.rs `balance_region`).
    /// A later band that received nothing (its first block moved to a new
    /// page) leaves no trace on this page.
    fn close_band(&mut self, balanced: bool) {
        let has_bands = self.cur.as_ref().is_some_and(|p| !p.bands.is_empty());
        let mut b = {
            let page = self.cur.as_mut().expect("a page is open");
            std::mem::replace(&mut page.open, BandB::new(usize::MAX, 0))
        };
        b.close_column(b.pos);
        if b.cols.is_empty() {
            if has_bands {
                return;
            }
            // The page's first band still carries one (empty) column range.
            b.cols.push((b.pos, b.pos));
        }
        if balanced {
            let sb = &self.sections[b.section];
            if sb.col_w.len() >= 2 && all_equal(&sb.col_w) {
                let wpx = sb.col_w[0].max(0) as f32 / 15.0;
                let span = (b.cols[0].0, b.pos);
                let heights: Vec<f32> = (span.0..span.1)
                    .map(|i| crate::block_height_est(&self.body[i], wpx))
                    .collect();
                if let Some((cols, height)) = balance_band(&heights, span, sb.col_w.len()) {
                    if height + 0.001 < b.tallest {
                        b.cols = cols;
                        b.tallest = height;
                    }
                }
            }
        }
        let height = b.tallest;
        let page = self.cur.as_mut().expect("a page is open");
        page.used_h += height;
        page.bands.push(Band {
            section: b.section,
            cols: b.cols,
        });
    }

    /// Close the page under construction, if any.
    fn finish_page(&mut self) {
        if self.cur.is_some() {
            self.close_band(false);
            let page = self.cur.take().expect("a page is open");
            self.pages.push(Page {
                bands: page.bands,
                filler: false,
            });
        }
    }

    /// The open band's remaining height (px): the sheet's content height
    /// less the closed bands above it.
    fn capacity(&self) -> f32 {
        let page = self.cur.as_ref().expect("a page is open");
        let owner = &self.sections[page.owner()];
        let content_h = (owner.h - owner.top - owner.bottom).max(1) as f32 / 15.0;
        content_h - page.used_h
    }

    /// Pour a section's block range into the open band, closing columns and
    /// pages as they fill; the block heights are estimated at the width of
    /// the column they land in. A band's first block may overflow only a
    /// fresh sheet (a block taller than the page still gets its own page);
    /// under closed bands it moves the band to a new page instead.
    fn pour(&mut self, i: usize, range: std::ops::Range<usize>) {
        let col_w: Vec<f32> = self.sections[i]
            .col_w
            .iter()
            .map(|&w| w.max(0) as f32 / 15.0)
            .collect();
        let ncols = col_w.len().max(1);
        let mut col = 0usize;
        for idx in range {
            loop {
                if self.cur.is_none() {
                    // A page break, an overflow, or a fresh section opens a
                    // new page for the rest of this section.
                    self.open_page(i, idx, false);
                    col = 0;
                }
                let (acc, col_start, first_of_band, has_bands) = {
                    let page = self.cur.as_ref().unwrap();
                    let b = &page.open;
                    (
                        b.acc,
                        b.col_start,
                        b.cols.is_empty() && idx == b.col_start,
                        !page.bands.is_empty(),
                    )
                };
                let bh = crate::block_height_est(&self.body[idx], col_w[col]);
                if acc + bh > self.capacity() && (idx > col_start || (first_of_band && has_bands)) {
                    if idx > col_start {
                        self.cur.as_mut().unwrap().open.close_column(idx);
                        col += 1;
                        if col >= ncols {
                            // The band's last column closed: the page is full.
                            self.finish_page();
                        }
                    } else {
                        // The first block of a later band does not fit under
                        // the closed bands: drop the empty band and move to a
                        // new page (never leave an empty band or a page that
                        // holds only such a band).
                        self.cur.as_mut().unwrap().open = BandB::new(usize::MAX, 0);
                        self.finish_page();
                    }
                    continue;
                }
                self.cur.as_mut().unwrap().open.acc += bh;
                break;
            }
            self.cur.as_mut().unwrap().open.pos = idx + 1;
            if crate::has_page_break(&self.body[idx]) {
                // A hard break ends the column and the page.
                self.cur.as_mut().unwrap().open.close_column(idx + 1);
                self.finish_page();
                col = 0;
            }
        }
    }
}

/// Flow a shown body into pages, each section with its own page size,
/// margins, gutter and columns, each section starting as its `w:type` says
/// (`nextPage`, `continuous`/`nextColumn`, `oddPage`, `evenPage`). The
/// result always has at least one page, like the old `paginate`'s
/// `vec![(0, len)]`.
pub(crate) fn flow(body: &[Block], last_sect: &str, gutter_at_top: bool) -> PageFlow {
    let ranges = split(body, last_sect);
    let n = ranges.len();
    let mut f = Flow {
        sections: ranges
            .iter()
            .map(|(_, sect)| section_box(sect, gutter_at_top))
            .collect(),
        body,
        pages: Vec::new(),
        next_number: 1,
        cur: None,
    };
    for (i, (range, _)) in ranges.into_iter().enumerate() {
        // An empty final section adds no page (a trailing sectPr's own block
        // is still part of a non-empty final range, so that does start one).
        if i == n - 1 && i > 0 && range.is_empty() {
            continue;
        }
        match f.sections[i].start {
            _ if i == 0 => f.open_page(i, range.start, true),
            SectionStart::NextPage => {
                f.finish_page();
                f.open_page(i, range.start, true);
            }
            SectionStart::Continuous | SectionStart::NextColumn => {
                let same = f.cur.as_ref().is_some_and(|p| {
                    let o = &f.sections[p.owner()];
                    (o.w, o.h) == (f.sections[i].w, f.sections[i].h)
                });
                if same {
                    // Same paper: balance the ending band, open this
                    // section's band below it on the same page.
                    f.close_band(true);
                    if let Some(start) = f.sections[i].num_start {
                        f.next_number = start;
                    }
                    let page = f.cur.as_mut().expect("a page is open");
                    page.open = BandB::new(i, range.start);
                } else {
                    f.finish_page();
                    f.open_page(i, range.start, true);
                }
            }
            SectionStart::OddPage | SectionStart::EvenPage => {
                f.finish_page();
                let want_odd = f.sections[i].start == SectionStart::OddPage;
                let num = f.sections[i].num_start.unwrap_or(f.next_number);
                if (num % 2 == 1) != want_odd {
                    // The filler belongs to the previous section, like
                    // export.rs `new_page(i - 1, false, true)`.
                    f.next_number = f.next_number.saturating_add(1);
                    f.pages.push(Page {
                        bands: vec![Band {
                            section: i - 1,
                            cols: vec![(range.start, range.start)],
                        }],
                        filler: true,
                    });
                }
                f.open_page(i, range.start, true);
            }
        }
        if !range.is_empty() {
            f.pour(i, range);
        }
    }
    f.finish_page();
    debug_assert!(!f.pages.is_empty());
    PageFlow {
        sections: f.sections,
        pages: f.pages,
    }
}

#[cfg(test)]
mod tests;
