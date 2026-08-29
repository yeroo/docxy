//! Page-view-only watermark layout.
//!
//! The output is an independent screen overlay. It never enters rendered
//! document lines or caret maps, so it cannot affect editing, hit testing,
//! selection, copy, export, or saved OOXML.

use docxcore::model::{Block, Document};
use docxcore::package::{HeaderVariant, Package, Watermark, WatermarkKind, watermark_label_from};
use docxcore::render::{Line, PageBox};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    marks: Vec<Watermark>,
    title_page_sections: Vec<bool>,
    even_odd: bool,
    /// Body section properties used to build `marks`. `None` is reserved for
    /// synthetic renderer-test states that are not tied to a live document.
    section_breaks: Option<Vec<String>>,
}

impl State {
    #[cfg(test)]
    pub(crate) fn new(
        marks: Vec<Watermark>,
        title_page_sections: Vec<bool>,
        even_odd: bool,
    ) -> Self {
        Self {
            marks,
            title_page_sections,
            even_odd,
            section_breaks: None,
        }
    }

    pub(crate) fn from_package(pkg: &Package) -> Self {
        let section_breaks = pkg
            .document
            .body
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(p) => p.props.section_break.as_deref(),
                Block::Table(_) | Block::Raw(_) => None,
            })
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut title_page_sections = section_breaks
            .iter()
            .map(|sect_pr| flag_on(sect_pr, "titlePg"))
            .collect::<Vec<_>>();
        title_page_sections.push(flag_on(pkg.sect_pr(), "titlePg"));

        let even_odd = pkg.has_even_odd();

        Self {
            marks: pkg.watermarks(),
            title_page_sections,
            even_odd,
            section_breaks: Some(section_breaks),
        }
    }

    pub(crate) fn label(&self) -> Option<String> {
        watermark_label_from(&self.marks)
    }

    pub(crate) fn matches_document_sections(&self, document: &Document) -> bool {
        let Some(expected) = &self.section_breaks else {
            return true;
        };
        expected
            .iter()
            .map(String::as_str)
            .eq(document.body.iter().filter_map(|block| match block {
                Block::Paragraph(paragraph) => paragraph.props.section_break.as_deref(),
                Block::Table(_) | Block::Raw(_) => None,
            }))
    }

    #[cfg(test)]
    pub(crate) fn mark_count(&self) -> usize {
        self.marks.len()
    }

    fn variant_for(&self, page: &PageBox) -> HeaderVariant {
        if page.section_page_index == 0
            && self
                .title_page_sections
                .get(page.section_index)
                .copied()
                .unwrap_or(false)
        {
            HeaderVariant::First
        } else if self.even_odd && page.document_page_index % 2 == 1 {
            HeaderVariant::Even
        } else {
            HeaderVariant::Default
        }
    }
}

/// One centered label to paint over a page. Coordinates are in the complete
/// rendered document, before viewport scrolling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Overlay {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) text: String,
}

/// Build at most one deterministic, terminal-honest label for each page.
pub(crate) fn layout(state: &State, pages: &[PageBox], lines: &[Line]) -> Vec<Overlay> {
    let mut overlays = Vec::new();
    for page in pages {
        // A usable page needs two border cells/rows and at least one inner cell.
        let inner_width = page.cols.saturating_sub(2);
        let inner_rows = page.rows.saturating_sub(2);
        if inner_width == 0 || inner_rows == 0 {
            continue;
        }

        let variant = state.variant_for(page);
        let mut texts = Vec::new();
        let mut picture = false;
        let mut unsupported = false;
        for mark in state.marks.iter().filter(|mark| {
            mark.header.section_index == page.section_index && mark.header.variant == variant
        }) {
            match &mark.kind {
                WatermarkKind::Text(text) if !text.trim().is_empty() => {
                    if !texts.contains(text) {
                        texts.push(text.clone());
                    }
                }
                WatermarkKind::Picture => picture = true,
                WatermarkKind::Unknown => unsupported = true,
                WatermarkKind::Text(_) => {}
            }
        }
        if texts.is_empty() && !picture && !unsupported {
            continue;
        }

        let mut details = Vec::new();
        if !texts.is_empty() {
            details.push(texts.join(" · "));
        }
        if picture {
            details.push("picture preview unavailable".to_string());
        }
        if unsupported {
            details.push("unsupported preview unavailable".to_string());
        }
        let full = format!("[Watermark: {}]", details.join(" · "));
        let text = clip_with_ellipsis(&full, inner_width);
        let text_width = UnicodeWidthStr::width(text.as_str());
        // Prefer a wholly blank page row nearest the vertical center, so the
        // label never hides document/header/footer text. A completely full page
        // falls back to its top border, where the label remains page-associated
        // without obscuring content.
        let first_inner = page.row + 1;
        let end_inner = page.row + page.rows - 1;
        let center = first_inner + inner_rows / 2;
        let row = (first_inner..end_inner)
            .filter(|&row| {
                lines.get(row).is_some_and(|line| {
                    display_range_is_blank(&line.plain(), page.col + 1, inner_width)
                })
            })
            .min_by_key(|row| row.abs_diff(center))
            .unwrap_or(page.row);
        overlays.push(Overlay {
            row,
            col: page.col + 1 + inner_width.saturating_sub(text_width) / 2,
            text,
        });
    }
    overlays
}

fn display_range_is_blank(text: &str, start: usize, width: usize) -> bool {
    let end = start + width;
    let mut col = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        let grapheme_end = col + grapheme_width;
        if grapheme_end > start && col < end && !grapheme.chars().all(char::is_whitespace) {
            return false;
        }
        col = grapheme_end;
        if col >= end {
            break;
        }
    }
    true
}

/// Return the display suffix beginning at `left` columns. If `left` cuts
/// through a wide grapheme, the grapheme is omitted and `gap` preserves its
/// remaining screen column so the following text does not shift left.
pub(crate) fn suffix_after_cols(text: &str, left: usize) -> (usize, String) {
    if left == 0 {
        return (0, text.to_string());
    }
    let mut pos = 0usize;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let width = UnicodeWidthStr::width(grapheme);
        let end = pos + width;
        if end > left {
            if pos < left {
                let next = byte + grapheme.len();
                return (end - left, text[next..].to_string());
            }
            return (pos - left, text[byte..].to_string());
        }
        pos = end;
    }
    (0, String::new())
}

fn clip_with_ellipsis(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let target = max_width - 1;
    let mut width = 0usize;
    let mut out = String::new();
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > target {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out.push('…');
    out
}

/// Whether an OOXML on/off element is present and not explicitly disabled.
fn flag_on(xml: &str, tag: &str) -> bool {
    let needle = format!("<w:{tag}");
    let Some(start) = xml.find(&needle) else {
        return false;
    };
    let end = xml[start..]
        .find('>')
        .map(|offset| start + offset)
        .unwrap_or(xml.len());
    !matches!(
        docxcore::load::xml_attr_value(&xml[start..end], "w:val").as_deref(),
        Some("false" | "0" | "off")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::package::WatermarkHeader;

    fn mark(section: usize, variant: HeaderVariant, kind: WatermarkKind) -> Watermark {
        Watermark {
            kind,
            header: WatermarkHeader {
                section_index: section,
                variant,
                relationship_id: format!("rId{section}"),
                part_name: format!("word/header{section}.xml"),
                inherited: section > 0,
            },
        }
    }

    fn page(
        section: usize,
        section_page: usize,
        document_page: usize,
        row: usize,
        rows: usize,
        cols: usize,
    ) -> PageBox {
        PageBox {
            section_index: section,
            section_page_index: section_page,
            document_page_index: document_page,
            row,
            col: 3,
            rows,
            cols,
        }
    }

    fn blank_lines(count: usize) -> Vec<Line> {
        vec![Line::default(); count]
    }

    #[test]
    fn multi_page_sections_select_only_the_applied_header_variant() {
        let state = State {
            marks: vec![
                mark(
                    0,
                    HeaderVariant::Default,
                    WatermarkKind::Text("DEFAULT ZERO".to_string()),
                ),
                mark(
                    0,
                    HeaderVariant::First,
                    WatermarkKind::Text("FIRST ZERO".to_string()),
                ),
                mark(0, HeaderVariant::Even, WatermarkKind::Picture),
                mark(
                    1,
                    HeaderVariant::Default,
                    WatermarkKind::Text("INHERITED ONE".to_string()),
                ),
            ],
            title_page_sections: vec![true, false],
            even_odd: true,
            section_breaks: None,
        };
        let pages = vec![
            page(0, 0, 0, 0, 20, 50),
            page(0, 1, 1, 21, 20, 50),
            page(1, 0, 2, 42, 20, 50),
            page(1, 1, 3, 63, 20, 50),
        ];

        let overlays = layout(&state, &pages, &blank_lines(100));

        assert_eq!(overlays.len(), 3);
        assert!(overlays[0].text.contains("FIRST ZERO"));
        assert!(overlays[1].text.contains("picture preview unavailable"));
        assert!(overlays[2].text.contains("INHERITED ONE"));
        assert!(
            overlays
                .iter()
                .all(|overlay| !overlay.text.contains("DEFAULT ZERO")),
            "the default header must not leak onto first/even pages: {overlays:?}"
        );
    }

    #[test]
    fn picture_and_unsupported_fallbacks_are_specific() {
        let state = State {
            marks: vec![
                mark(0, HeaderVariant::Default, WatermarkKind::Picture),
                mark(0, HeaderVariant::Default, WatermarkKind::Unknown),
            ],
            title_page_sections: vec![false],
            even_odd: false,
            section_breaks: None,
        };

        let overlays = layout(&state, &[page(0, 0, 0, 0, 20, 80)], &blank_lines(20));

        assert_eq!(overlays.len(), 1);
        assert!(overlays[0].text.contains("picture preview unavailable"));
        assert!(overlays[0].text.contains("unsupported preview unavailable"));
    }

    #[test]
    fn unicode_and_tiny_pages_clip_by_display_width_without_splitting_graphemes() {
        let state = State {
            marks: vec![mark(
                0,
                HeaderVariant::Default,
                WatermarkKind::Text("機密 e\u{301} — ПРОЕКТ".to_string()),
            )],
            title_page_sections: vec![false],
            even_odd: false,
            section_breaks: None,
        };
        let pages = [
            page(0, 0, 0, 0, 5, 16),
            page(0, 1, 1, 6, 2, 2),
            page(0, 2, 2, 9, 3, 3),
        ];

        let overlays = layout(&state, &pages, &blank_lines(12));

        assert_eq!(
            overlays.len(),
            2,
            "a border-only page has no safe overlay row"
        );
        assert!(UnicodeWidthStr::width(overlays[0].text.as_str()) <= 14);
        assert!(overlays[0].text.ends_with('…'));
        assert_eq!(overlays[1].text, "…");
    }

    #[test]
    fn no_watermark_produces_no_overlay() {
        let state = State {
            title_page_sections: vec![false],
            ..State::default()
        };
        assert!(layout(&state, &[page(0, 0, 0, 0, 20, 80)], &blank_lines(20)).is_empty());
    }

    #[test]
    fn a_full_page_uses_the_frame_instead_of_hiding_document_text() {
        let state = State {
            marks: vec![mark(
                0,
                HeaderVariant::Default,
                WatermarkKind::Text("DENSE".to_string()),
            )],
            title_page_sections: vec![false],
            even_odd: false,
            section_breaks: None,
        };
        let page = page(0, 0, 0, 0, 5, 20);
        let lines = vec![
            Line {
                spans: vec![docxcore::render::Span {
                    text: "XXXXXXXXXXXXXXXXXXXXXXX".to_string(),
                    style: docxcore::render::Style::default(),
                    link: None,
                }],
            };
            5
        ];

        let overlays = layout(&state, std::slice::from_ref(&page), &lines);

        assert_eq!(overlays[0].row, page.row);
    }

    #[test]
    fn horizontal_clipping_never_returns_half_a_wide_grapheme() {
        assert_eq!(suffix_after_cols("A機密", 2), (1, "密".to_string()));
        assert_eq!(suffix_after_cols("A機密", 3), (0, "密".to_string()));
    }
}
