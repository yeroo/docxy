//! Columns (#2257): a page set in two or three columns becomes a section
//! of that many columns. PdfCraft's export flows every page as one column
//! (and may take columns of short lines for a table), so the page is split
//! here, before the export, into **virtual pages**: the full-width band
//! above the columns, on the real page's size, then the column region with
//! its columns stacked one under the other on a page as wide as one column.
//! The export then reads that region as one column, in the columns' order,
//! and makes one section of each virtual page; `convert.rs` gives the
//! region's section its columns, a continuous start and the real page's
//! size back.
//!
//! The region is the largest run of blocks, from the bottom of the page
//! up, whose boxes leave 1 or 2 vertical gutters (at least `max(12 pt,
//! body size)` wide, in the middle 80 % of the text's width) and so make 2
//! or 3 bands of similar width (within 20 % of their mean), each holding at
//! least a fifth of the region's text height and filled at least half by
//! its lines. Above it, a block either crosses a gutter (a heading over the
//! columns) or lies in one band only (a heading over the first column);
//! blocks in several bands there mean text between column runs, and the
//! page is left as PdfCraft flows it. So is a page the export positions,
//! one with ruled lines in the region (a ruled table, or rules between the
//! columns), and one with more than [`MAX_BLOCKS`] blocks.

use super::Source;
use pdfcraft_export::{Block, Image, Page};

/// The most blocks a page may have for its columns to be looked for.
pub(crate) const MAX_BLOCKS: usize = 4000;
/// The most blocks above the columns.
const MAX_BAND: usize = 64;

/// One page given to the export.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Virtual {
    pub(crate) page: Page,
    /// The source of each of `page.blocks`.
    pub(crate) sources: Vec<Source>,
    /// The columns of its section: how many, and the space between them
    /// (points).
    pub(crate) cols: Option<(usize, f64)>,
    /// It starts where the virtual page before it ends (the columns under
    /// a full-width band), not on a new page.
    pub(crate) continuous: bool,
    /// The real page's size (points), which a column region's section
    /// takes back.
    pub(crate) real: [f64; 2],
    /// The real page's index.
    pub(crate) from: usize,
}

/// What [`detect`] found on a page.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    /// The bands' x ranges, left to right.
    pub(crate) bands: Vec<[f64; 2]>,
    /// Each block's band, `None` for a block above the columns.
    pub(crate) block_band: Vec<Option<usize>>,
    /// Each image's band, `None` for one above the columns.
    pub(crate) image_band: Vec<Option<usize>>,
    /// The region's top (points, y up).
    pub(crate) top: f64,
}

/// `page` as the export takes it: itself, or its band and its column
/// region.
pub(crate) fn split(page: Page, sources: Vec<Source>, from: usize) -> Vec<Virtual> {
    let real = [page.width, page.height];
    let Some(layout) = detect(&page) else {
        return vec![Virtual {
            page,
            sources,
            cols: None,
            continuous: false,
            real,
            from,
        }];
    };
    let body = body_size(&page);
    let n = layout.bands.len();
    let mut band_blocks = Vec::new();
    let mut band_sources = Vec::new();
    let mut cols: Vec<Vec<(Block, Source)>> = vec![Vec::new(); n];
    for ((b, s), at) in page.blocks.into_iter().zip(sources).zip(&layout.block_band) {
        match at {
            Some(j) => cols[*j].push((b, s)),
            None => {
                band_blocks.push(b);
                band_sources.push(s);
            }
        }
    }
    let mut band_images = Vec::new();
    let mut col_images: Vec<Vec<Image>> = vec![Vec::new(); n];
    for (im, at) in page.images.into_iter().zip(&layout.image_band) {
        match at {
            Some(j) => col_images[*j].push(im),
            None => band_images.push(im),
        }
    }
    // Rules are only above the region (`detect` refuses one in it).
    let band_rules = page.rules;
    let colw = layout.bands.iter().map(|b| b[1] - b[0]).fold(0.0, f64::max);
    let gutter = layout
        .bands
        .windows(2)
        .map(|w| w[1][0] - w[0][1])
        .sum::<f64>()
        / (n - 1) as f64;
    // PdfCraft's margin is min(width / 8, 1 in): the width whose text area
    // is one column.
    let width = if colw + 144.0 >= 576.0 {
        colw + 144.0
    } else {
        colw * 4.0 / 3.0
    };
    let margin = (width / 8.0).min(72.0);
    let extent = |blocks: &[(Block, Source)], images: &[Image]| {
        let rects = blocks
            .iter()
            .map(|(b, _)| b.rect)
            .chain(images.iter().map(|i| i.rect));
        rects.fold([f64::INFINITY, f64::NEG_INFINITY], |a, r| {
            [a[0].min(r[1]), a[1].max(r[3])]
        })
    };
    let gap = body * 0.3;
    let heights: Vec<f64> = (0..n)
        .map(|j| {
            let [lo, hi] = extent(&cols[j], &col_images[j]);
            if hi >= lo { hi - lo } else { 0.0 }
        })
        .collect();
    // Tall enough that its margins are PdfCraft's full inch, and everything
    // stays on it.
    let height = (heights.iter().sum::<f64>() + gap * (n - 1) as f64 + 144.0).max(576.0);
    let mut cursor = height - 72.0;
    let mut blocks = Vec::new();
    let mut region_sources = Vec::new();
    let mut images = Vec::new();
    for j in 0..n {
        let [_, hi] = extent(&cols[j], &col_images[j]);
        if !hi.is_finite() {
            continue;
        }
        let dx = margin - layout.bands[j][0];
        let dy = cursor - hi;
        let shift = |r: [f64; 4]| [r[0] + dx, r[1] + dy, r[2] + dx, r[3] + dy];
        for (mut b, s) in std::mem::take(&mut cols[j]) {
            b.rect = shift(b.rect);
            blocks.push(b);
            region_sources.push(s);
        }
        for mut im in std::mem::take(&mut col_images[j]) {
            im.rect = shift(im.rect);
            images.push(im);
        }
        cursor -= heights[j] + gap;
    }
    let region = Virtual {
        page: Page {
            width,
            height,
            blocks,
            images,
            background: page.background,
            rules: Vec::new(),
        },
        sources: region_sources,
        cols: Some((n, gutter)),
        continuous: false,
        real,
        from,
    };
    if band_blocks.is_empty() && band_images.is_empty() {
        return vec![region];
    }
    let band = Virtual {
        page: Page {
            width: real[0],
            height: real[1],
            blocks: band_blocks,
            images: band_images,
            background: page.background,
            rules: band_rules,
        },
        sources: band_sources,
        cols: None,
        continuous: false,
        real,
        from,
    };
    vec![
        band,
        Virtual {
            continuous: true,
            ..region
        },
    ]
}

/// The page's columns, if it is set in 2 or 3 (see the module's comment).
pub(crate) fn detect(page: &Page) -> Option<Layout> {
    let blocks = &page.blocks;
    if blocks.len() < 2 || blocks.len() > MAX_BLOCKS || positioned(page) {
        return None;
    }
    let finite = |r: &[f64; 4]| r.iter().all(|v| v.is_finite()) && r[2] >= r[0] && r[3] >= r[1];
    if !blocks.iter().all(|b| finite(&b.rect)) || !page.images.iter().all(|i| finite(&i.rect)) {
        return None;
    }
    let body = body_size(page).max(1.0);
    let left = blocks
        .iter()
        .map(|b| b.rect[0])
        .fold(f64::INFINITY, f64::min);
    let right = blocks
        .iter()
        .map(|b| b.rect[2])
        .fold(f64::NEG_INFINITY, f64::max);
    let span = right - left;
    if span <= 0.0 {
        return None;
    }
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by(|&a, &b| blocks[b].rect[3].total_cmp(&blocks[a].rect[3]));
    for k in 0..=MAX_BAND.min(order.len().saturating_sub(2)) {
        let region = &order[k..];
        let top = region
            .iter()
            .map(|&i| blocks[i].rect[3])
            .fold(f64::NEG_INFINITY, f64::max);
        let in_region = |r: &[f64; 4]| r[3] <= top + 1.0;
        let images: Vec<usize> = (0..page.images.len())
            .filter(|&i| in_region(&page.images[i].rect))
            .collect();
        let mut spans: Vec<[f64; 2]> = region
            .iter()
            .map(|&i| [blocks[i].rect[0], blocks[i].rect[2]])
            .chain(
                images
                    .iter()
                    .map(|&i| [page.images[i].rect[0], page.images[i].rect[2]]),
            )
            .collect();
        spans.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let mut merged: Vec<[f64; 2]> = Vec::new();
        for s in spans {
            match merged.last_mut() {
                Some(m) if s[0] <= m[1] => m[1] = m[1].max(s[1]),
                _ => merged.push(s),
            }
        }
        // Bands between the gutters: wide gaps in the middle 80 %.
        let min_gap = body.max(12.0);
        let mut bands: Vec<[f64; 2]> = Vec::new();
        let mut start = merged.first()?[0];
        for w in merged.windows(2) {
            let (a, b) = (w[0][1], w[1][0]);
            let mid = (a + b) / 2.0;
            if b - a >= min_gap && mid > left + span * 0.1 && mid < right - span * 0.1 {
                bands.push([start, a]);
                start = b;
            }
        }
        bands.push([start, merged.last()?[1]]);
        if !(2..=3).contains(&bands.len()) {
            continue;
        }
        let widths: Vec<f64> = bands.iter().map(|b| b[1] - b[0]).collect();
        let mean = widths.iter().sum::<f64>() / widths.len() as f64;
        if mean <= 0.0 || widths.iter().any(|w| (w - mean).abs() > mean * 0.2) {
            continue;
        }
        let band_of = |r: &[f64; 4]| {
            bands
                .iter()
                .position(|b| r[0] >= b[0] - 0.5 && r[2] <= b[1] + 0.5)
        };
        let mut block_band = vec![None; blocks.len()];
        let mut height = vec![0.0; bands.len()];
        let mut filled = vec![0.0; bands.len()];
        let mut lines = vec![0usize; bands.len()];
        for &i in region {
            let r = &blocks[i].rect;
            let j = band_of(r)?;
            block_band[i] = Some(j);
            height[j] += r[3] - r[1];
            filled[j] += r[2] - r[0];
            lines[j] += 1;
        }
        let total: f64 = height.iter().sum();
        if total <= 0.0
            || height.iter().any(|h| h / total < 0.2)
            || (0..bands.len()).any(|j| filled[j] / (lines[j] as f64 * widths[j]) < 0.5)
        {
            continue;
        }
        let bottom = region
            .iter()
            .map(|&i| blocks[i].rect[1])
            .fold(f64::INFINITY, f64::min);
        // A ruled table, or rules between the columns: left as it is.
        let crosses = |r: &[f64; 4]| r[1] < top && r[3] > bottom && r[0] < right && r[2] > left;
        if page.rules.iter().any(crosses) {
            return None;
        }
        let mut image_band = vec![None; page.images.len()];
        for &i in &images {
            image_band[i] = Some(band_of(&page.images[i].rect)?);
        }
        // Above the region: nothing reaches down into it, and what does not
        // cross a gutter is in one band only.
        let gutters: Vec<[f64; 2]> = bands.windows(2).map(|w| [w[0][1], w[1][0]]).collect();
        let crosses_gutter = |r: &[f64; 4]| gutters.iter().any(|g| r[0] < g[1] && r[2] > g[0]);
        let mut beside = None;
        for &i in &order[..k] {
            let r = &blocks[i].rect;
            if r[1] < top - 1.0 {
                return None;
            }
            if !crosses_gutter(r) {
                let j = bands.iter().position(|b| r[0] < b[1] && r[2] > b[0]);
                if beside.is_some_and(|b| Some(b) != j) {
                    return None;
                }
                beside = j;
            }
        }
        let mut layout = Layout {
            bands,
            block_band,
            image_band,
            top,
        };
        lift_heads(&mut layout, blocks, beside);
        if layout.block_band.iter().flatten().count() < 2 {
            return None;
        }
        return Some(layout);
    }
    None
}

/// Blocks of the region wholly above where every column has begun (a
/// heading over the first column) belong to the band above: unless the
/// band already has text beside another column.
fn lift_heads(layout: &mut Layout, blocks: &[Block], beside: Option<usize>) {
    let n = layout.bands.len();
    let mut tops = vec![f64::NEG_INFINITY; n];
    let mut seconds = vec![f64::NEG_INFINITY; n];
    for (b, at) in blocks.iter().zip(&layout.block_band) {
        if let Some(j) = *at {
            let t = b.rect[3];
            if t > tops[j] {
                seconds[j] = tops[j];
                tops[j] = t;
            } else if t > seconds[j] {
                seconds[j] = t;
            }
        }
    }
    let start = tops.iter().copied().fold(f64::INFINITY, f64::min);
    if !start.is_finite() {
        return;
    }
    for (b, at) in blocks.iter().zip(layout.block_band.iter_mut()) {
        if let Some(j) = *at {
            let lifted = b.rect[1] >= start - 1.0 && beside.is_none_or(|k| k == j);
            // A column keeps at least its own text.
            if lifted && seconds[j].is_finite() {
                *at = None;
            }
        }
    }
    let lifted_top = blocks
        .iter()
        .zip(&layout.block_band)
        .filter(|(_, at)| at.is_some())
        .map(|(b, _)| b.rect[3])
        .fold(f64::NEG_INFINITY, f64::max);
    if lifted_top.is_finite() {
        layout.top = lifted_top;
    }
}

/// The text size most of the page's characters are set in
/// (`pdfcraft_export`'s `body_size`, for one page; not public).
pub(crate) fn body_size(page: &Page) -> f64 {
    let mut by: Vec<(i64, usize)> = Vec::new();
    for b in &page.blocks {
        let k = (b.size * 2.0).round() as i64;
        match by.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 = e.1.saturating_add(b.text.len()),
            None => by.push((k, b.text.len())),
        }
        if by.len() > 256 {
            break;
        }
    }
    let mut best: Option<(i64, usize)> = None;
    for e in by {
        if best.is_none_or(|b| e.1 > b.1) {
            best = Some(e);
        }
    }
    best.map_or(11.0, |e| e.0 as f64 / 2.0)
}

/// Whether the export keeps `page` as positioned text boxes rather than
/// flowing it: rotated text, or text overlapped by a picture or by text
/// twice the body size. `pdfcraft_export`'s `as_word::positioned`, which
/// is not public (its budget too).
pub(crate) fn positioned(page: &Page) -> bool {
    if page.blocks.iter().any(|b| b.rotation.abs() > 0.1) {
        return true;
    }
    let body = body_size(page).max(1.0);
    let overlays = page.images.iter().map(|im| (im.rect, None)).chain(
        page.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.size >= 2.0 * body)
            .map(|(i, b)| (b.rect, Some(i))),
    );
    let mut budget: usize = 1_000_000;
    for (r, own) in overlays {
        for (i, b) in page.blocks.iter().enumerate() {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            if Some(i) != own && overlaps(r, b.rect) {
                return true;
            }
        }
    }
    false
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    let w = a[2].min(b[2]) - a[0].max(b[0]);
    let h = a[3].min(b[3]) - a[1].max(b[1]);
    w.is_finite() && h.is_finite() && w > 0.5 && h > 0.5
}

#[cfg(test)]
mod tests {
    use super::super::running::block;
    use super::*;

    fn letter(blocks: Vec<Block>) -> Page {
        Page {
            width: 612.0,
            height: 792.0,
            blocks,
            ..Page::default()
        }
    }

    fn line(x0: f64, y: f64, w: f64) -> Block {
        block(
            "A line of column text that fills it",
            [x0, y, x0 + w, y + 10.0],
            10.0,
        )
    }

    /// Two columns of `n` lines, x 72-288 and 324-540.
    fn two_columns(n: usize, top: f64) -> Vec<Block> {
        let mut out = Vec::new();
        for i in 0..n {
            let y = top - i as f64 * 13.0;
            out.push(line(72.0, y, 210.0));
            out.push(line(324.0, y, 214.0));
        }
        out
    }

    fn sources(page: &Page) -> Vec<Source> {
        page.blocks
            .iter()
            .map(|_| Source {
                base_font: "Times-Roman".into(),
                color: [0.0; 3],
            })
            .collect()
    }

    #[test]
    fn two_columns_are_found() {
        let page = letter(two_columns(12, 650.0));
        let l = detect(&page).unwrap();
        assert_eq!(l.bands.len(), 2);
        assert!(l.block_band.iter().all(|b| b.is_some()));
        assert_eq!(l.block_band[0], Some(0));
        assert_eq!(l.block_band[1], Some(1));
    }

    #[test]
    fn a_heading_across_the_gutter_is_the_band_above() {
        let mut blocks = vec![block(
            "Observations by site across the valley",
            [72.0, 686.0, 420.0, 704.0],
            16.0,
        )];
        blocks.extend(two_columns(12, 650.0));
        let page = letter(blocks);
        let l = detect(&page).unwrap();
        assert_eq!(l.block_band[0], None);
        assert!(l.block_band[1..].iter().all(|b| b.is_some()));
        let v = split(page.clone(), sources(&page), 1);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].cols, v[0].continuous), (None, false));
        assert_eq!(v[0].page.blocks.len(), 1);
        assert_eq!([v[0].page.width, v[0].page.height], [612.0, 792.0]);
        let (n, gutter) = v[1].cols.unwrap();
        assert_eq!(n, 2);
        assert!((gutter - 42.0).abs() < 0.01, "{gutter}");
        assert!(v[1].continuous);
        assert_eq!(v[1].real, [612.0, 792.0]);
        assert_eq!(v[1].from, 1);
    }

    #[test]
    fn a_heading_over_the_first_column_is_lifted_into_the_band() {
        // columns.pdf's layout: the heading does not reach the gutter.
        let mut blocks = vec![block("Two columns", [54.0, 715.0, 186.0, 737.0], 18.0)];
        blocks.push(block("Left column text", [54.0, 327.0, 188.0, 690.0], 9.0));
        blocks.push(block(
            "Right column text",
            [324.0, 327.0, 473.0, 690.0],
            9.0,
        ));
        let l = detect(&letter(blocks)).unwrap();
        assert_eq!(l.block_band, vec![None, Some(0), Some(1)]);
    }

    #[test]
    fn linearized_columns_are_stacked_on_a_page_one_column_wide() {
        let page = letter(two_columns(12, 650.0));
        let v = split(page.clone(), sources(&page), 0);
        assert_eq!(v.len(), 1);
        let p = &v[0].page;
        // Widest band 214 pt: PdfCraft's text area on the page is 214 pt.
        let margin = (p.width / 8.0).min(72.0);
        assert!((p.width - 2.0 * margin - 214.0).abs() < 1e-9, "{}", p.width);
        let left: Vec<&Block> = p.blocks[..12].iter().collect();
        let right: Vec<&Block> = p.blocks[12..].iter().collect();
        assert!(
            p.blocks.iter().all(|b| (b.rect[0] - margin).abs() < 1e-9),
            "one left edge"
        );
        // The right column is all below the left one, in its own order.
        let lowest_left = left.iter().map(|b| b.rect[1]).fold(f64::INFINITY, f64::min);
        assert!(right.iter().all(|b| b.rect[3] < lowest_left));
        assert!(right.windows(2).all(|w| w[0].rect[1] > w[1].rect[1]));
        // Everything is on the page.
        assert!(
            p.blocks
                .iter()
                .all(|b| b.rect[1] >= 0.0 && b.rect[3] <= p.height)
        );
        assert_eq!(v[0].sources.len(), 24);
        // PdfCraft sees one column: no table out of the two.
        assert!(!v[0].continuous);
    }

    #[test]
    fn a_narrow_column_page_keeps_its_text_area() {
        let mut blocks = Vec::new();
        for i in 0..10 {
            let y = 650.0 - i as f64 * 13.0;
            blocks.push(line(72.0, y, 120.0));
            blocks.push(line(240.0, y, 120.0));
        }
        let page = letter(blocks);
        let v = split(page.clone(), sources(&page), 0);
        let w = v[0].page.width;
        assert_eq!(w, 120.0 * 4.0 / 3.0);
        assert_eq!(w - 2.0 * (w / 8.0), 120.0);
    }

    #[test]
    fn interleaved_or_single_columns_are_left_alone() {
        // One column.
        let one: Vec<Block> = (0..12)
            .map(|i| line(72.0, 650.0 - i as f64 * 13.0, 460.0))
            .collect();
        assert!(detect(&letter(one)).is_none());
        // Text across the page between two runs of columns.
        let mut blocks = two_columns(6, 650.0);
        blocks.push(block(
            "A paragraph right across the page",
            [72.0, 560.0, 540.0, 572.0],
            10.0,
        ));
        blocks.extend(two_columns(6, 540.0));
        assert!(detect(&letter(blocks)).is_none());
        // Columns with rules in them (a ruled table).
        let mut page = letter(two_columns(12, 650.0));
        page.rules.push([300.0, 500.0, 301.0, 660.0]);
        assert!(detect(&page).is_none());
        // Unequal columns.
        let mut blocks = Vec::new();
        for i in 0..12 {
            let y = 650.0 - i as f64 * 13.0;
            blocks.push(line(72.0, y, 100.0));
            blocks.push(line(220.0, y, 320.0));
        }
        assert!(detect(&letter(blocks)).is_none());
        // A table's short cells fill little of their bands.
        let mut blocks = Vec::new();
        for i in 0..12 {
            let y = 650.0 - i as f64 * 13.0;
            blocks.push(block("Item", [72.0, y, 95.0, y + 10.0], 10.0));
            blocks.push(block(
                "Value",
                [72.0 + 140.0, y, 212.0 + 25.0, y + 10.0],
                10.0,
            ));
            blocks.push(block("Price note", [310.0, y, 520.0, y + 10.0], 10.0));
        }
        assert!(detect(&letter(blocks)).is_none());
    }
}
