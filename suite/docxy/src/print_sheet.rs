//! Print Layout's page sheets, shared by a document tab and the embedded
//! [`crate::ext::DocView`] (#1888): one band's body content with its own
//! section's columns ([`band_el`]), the grey canvas under the sheets
//! ([`canvas_color`], [`CANVAS_PAD`], [`SHEET_GAP`]) and, for the embedded
//! view, a document's whole page plan ([`page_plan`]). The document tab
//! keeps its own plan (`tab_page_flow`, which flows the markup view's body
//! and reads its open header or footer); the view's is the same flow over
//! its live body, and a test holds the two equal.

use super::*;
use docxcore::editor::Editor;
use docxcore::package::{Package, SectionParts};
use std::collections::HashMap;

/// The canvas's padding above the first sheet and below the last, px.
pub(crate) const CANVAS_PAD: f32 = 24.;
/// The gap between two sheets, px.
pub(crate) const SHEET_GAP: f32 = 18.;
/// A sheet's 1 px border.
pub(crate) const SHEET_BORDER: u32 = 0xd0d0d0;

/// The canvas under the sheets: a dark page sits on a darker canvas, so
/// its edge shows (#2079); a light page on a dark theme on a dark grey,
/// else on the light grey.
pub(crate) fn canvas_color(page_dark: bool, theme_dark: bool) -> u32 {
    if page_dark {
        dark_page::CANVAS_DARK
    } else if theme_dark {
        0x2b2b2b
    } else {
        0x9a9a9a
    }
}

/// The middle content of one band: a single flow, or an N-column row (each
/// column its own block range) with the band's section's own column
/// widths, the space after each column, and the rule between them (in
/// `rule`) when it sets `w:sep`. The ranges are positional — pour never
/// skips a column — so column i draws range i, and a column the band didn't
/// reach draws empty at its configured width. `slot_for` gives a top-level
/// paragraph its line-number slot (a document tab's line numbers, #746);
/// the embedded view numbers none.
#[allow(clippy::too_many_arguments)]
pub(crate) fn band_el(
    band: &page_flow::Band,
    sect: &page_flow::SectionBox,
    body: &[Block],
    markers: &[Option<String>],
    ctx: RenderCtx,
    zoom: f32,
    rule: Hsla,
    slot_for: &dyn Fn(&Block) -> Option<LineSlot>,
) -> AnyElement {
    let tw = move |t: i32| px(zoom * (t.max(0) as f32) / 15.0);
    if sect.col_w.len() <= 1 {
        let (s, e) = band.cols.first().copied().unwrap_or((0, 0));
        let col_ctx = RenderCtx {
            line_width: Some(zoom * sect.single_col_w().max(0) as f32 / 15.0),
            ..ctx
        };
        let blocks: Vec<AnyElement> = (s..e)
            .map(|i| {
                block_el_numbered(
                    &body[i],
                    vec![i],
                    markers[i].as_deref(),
                    col_ctx,
                    slot_for(&body[i]),
                )
            })
            .collect();
        // One column still draws at the section's column width (an
        // explicit single `w:col`), which is the full text width for an
        // ordinary equal-width section.
        return v_flex()
            .w(tw(sect.single_col_w()))
            .flex_none()
            .gap_1()
            .children(blocks)
            .into_any_element();
    }
    let mut els: Vec<AnyElement> = Vec::new();
    for ci in 0..sect.col_w.len() {
        if ci > 0 {
            let gap = sect.col_gap.get(ci - 1).copied().unwrap_or(0).max(0);
            let mut g = div()
                .w(tw(gap))
                .flex_none()
                .self_stretch()
                .flex()
                .justify_center();
            if sect.sep {
                g = g.child(div().w(px(1.)).h_full().bg(rule));
            }
            els.push(g.into_any_element());
        }
        let (s, e) = band.cols.get(ci).copied().unwrap_or((0, 0));
        let col_ctx = RenderCtx {
            line_width: Some(zoom * sect.col_w[ci].max(0) as f32 / 15.0),
            ..ctx
        };
        let blocks: Vec<AnyElement> = (s..e)
            .map(|i| {
                block_el_numbered(
                    &body[i],
                    vec![i],
                    markers[i].as_deref(),
                    col_ctx,
                    slot_for(&body[i]),
                )
            })
            .collect();
        els.push(
            v_flex()
                .w(tw(sect.col_w[ci]))
                .flex_none()
                .gap_1()
                .children(blocks)
                .into_any_element(),
        );
    }
    h_flex()
        .w_full()
        .items_start()
        .children(els)
        .into_any_element()
}

/// The page box a header's (`is_header`) or footer's framed paragraphs
/// are placed in (#767), as the PDF export places them: on the sheet of
/// section box `sb`, numbered even or not. `dist` is that region's header
/// or footer distance. A footer's flow ends at the footer distance; its top
/// (what `vAnchor="text"` measures from) is the estimated height of the
/// part's unframed `flow`, at least a line, above it (the export measures
/// it). `hf_w` is the text area's width, px at `zoom`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn frame_geom(
    sb: &page_flow::SectionBox,
    is_header: bool,
    dist: i32,
    flow: &[(usize, &Block)],
    hf_w: f32,
    zoom: f32,
    even: bool,
) -> docxcore::frame::FrameGeom {
    let band_top = if is_header {
        dist
    } else {
        let flow_h = flow
            .iter()
            .map(|(_, b)| block_height_est(b, hf_w / zoom) * 15.0)
            .sum::<f32>() as i32;
        sb.h - dist - flow_h.max(264)
    };
    docxcore::frame::FrameGeom {
        page_w: sb.w,
        page_h: sb.h,
        left: sb.left,
        right: sb.right,
        top: sb.top,
        bottom: sb.bottom,
        band_top,
        even,
    }
}

/// The section a document's last pages take their geometry from: the
/// body's trailing sectPr, else the package's, else none (`""`: Letter,
/// 1" margins). Unlike a document tab's `final_sect_pr`, a document with
/// no package keeps its trailing sectPr: an HTML-imported mail's
/// 225-twip margins (#1928) stay its pages' margins.
pub(crate) fn last_sect<'a>(doc: &'a Document, pkg: Option<&'a Package>) -> &'a str {
    if let Some(s) = doc.trailing_section_properties() {
        return &s.raw;
    }
    pkg.map_or("", Package::sect_pr)
}

/// A document's Print Layout pages, as a document tab's Print Layout
/// splits them: the flow (each section with its own page size, margins,
/// gutter and columns, #745), each page's header/footer slot (#640) and
/// the parts every section applies, the shown parts' blocks, and each
/// page's PAGE / NUMPAGES / SECTIONPAGES (#1437).
pub(crate) struct PagePlan {
    pub flow: page_flow::PageFlow,
    pub slots: Vec<hf::PageSlot>,
    pub parts: Vec<SectionParts>,
    pub part_blocks: HashMap<String, Vec<Block>>,
    pub page_vals: Vec<page_fields::PageVals>,
}

impl PagePlan {
    /// The header (`is_header`) or footer part page `pi` shows, if any.
    pub(crate) fn part(&self, pi: usize, is_header: bool) -> Option<&str> {
        self.slots
            .get(pi)
            .and_then(|s| hf::slot_part(&self.parts, *s, is_header))
    }

    /// The blocks of the part page `pi` shows (none without one).
    pub(crate) fn blocks(&self, pi: usize, is_header: bool) -> &[Block] {
        self.part(pi, is_header)
            .and_then(|n| self.part_blocks.get(n))
            .map_or(&[], Vec::as_slice)
    }
}

/// The page count of `editor`'s live body (all markup) in Print Layout.
pub(crate) fn page_count(editor: &Editor, pkg: Option<&Package>) -> usize {
    let gutter_at_top = pkg.is_some_and(Package::has_gutter_at_top);
    page_flow::flow(&editor.doc.body, last_sect(&editor.doc, pkg), gutter_at_top)
        .pages
        .len()
}

/// [`PagePlan`] of `editor`'s live body, its parts from `pkg`.
pub(crate) fn page_plan(editor: &Editor, pkg: Option<&Package>) -> PagePlan {
    let doc = &editor.doc;
    let gutter_at_top = pkg.is_some_and(Package::has_gutter_at_top);
    let flow = page_flow::flow(&doc.body, last_sect(doc, pkg), gutter_at_top);
    let even_odd = pkg.is_some_and(Package::has_even_odd);
    let slots = hf::slots_of(editor, &flow, even_odd);
    let parts = pkg.map(|p| hf::resolve(editor, p)).unwrap_or_default();
    let mut part_blocks: HashMap<String, Vec<Block>> = HashMap::new();
    if let Some(p) = pkg {
        for s in &parts {
            for a in s.headers.iter().chain(&s.footers).flatten() {
                part_blocks
                    .entry(a.part_name.clone())
                    .or_insert_with(|| parse_hf_part(p, &a.part_name));
            }
        }
    }
    let styles = pkg
        .and_then(|p| p.part("word/styles.xml"))
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(docxcore::styles::parse_styles_xml)
        .unwrap_or_default();
    let headings = page_fields::chapter_headings(
        doc,
        &flow,
        pkg.and_then(|p| p.part("word/numbering.xml")),
        &styles,
    );
    let page_vals = page_fields::page_values(&flow, &headings);
    PagePlan {
        flow,
        slots,
        parts,
        part_blocks,
        page_vals,
    }
}

#[cfg(test)]
mod tests;
