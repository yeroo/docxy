//! Bidirectional visual-line projection for DOCX terminal layout.
//!
//! This module stays in `docxy` because it depends on `unicode-bidi` and is a
//! display-only projection. Callers pass one already-wrapped logical line; the
//! returned clusters are in terminal visual order while all offsets remain the
//! editor's logical character offsets.

#![allow(dead_code)]

use std::ops::Range;

use unicode_bidi::{BidiInfo, Level, ParagraphBidiInfo};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseDirection {
    Auto,
    Ltr,
    Rtl,
}

impl BaseDirection {
    fn level(self) -> Option<Level> {
        match self {
            BaseDirection::Auto => None,
            BaseDirection::Ltr => Some(Level::ltr()),
            BaseDirection::Rtl => Some(Level::rtl()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RunDirection {
    #[default]
    Natural,
    Ltr,
    Rtl,
    LtrOverride,
    RtlOverride,
}

impl RunDirection {
    fn open_control(self) -> Option<char> {
        match self {
            RunDirection::Natural => None,
            RunDirection::Ltr => Some('\u{202a}'),
            RunDirection::Rtl => Some('\u{202b}'),
            RunDirection::LtrOverride => Some('\u{202d}'),
            RunDirection::RtlOverride => Some('\u{202e}'),
        }
    }
}

const PDF: char = '\u{202c}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogicalChar<T> {
    pub ch: char,
    pub display: Option<String>,
    pub logical_offset: Option<usize>,
    pub owner: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectionalRange {
    start: usize,
    end: usize,
    direction: RunDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualLineBuilder<T> {
    base: BaseDirection,
    chars: Vec<LogicalChar<T>>,
    directions: Vec<DirectionalRange>,
}

impl<T> VisualLineBuilder<T> {
    pub(crate) fn new(base: BaseDirection) -> Self {
        Self {
            base,
            chars: Vec::new(),
            directions: Vec::new(),
        }
    }

    pub(crate) fn push_char(
        &mut self,
        ch: char,
        display: Option<String>,
        logical_offset: Option<usize>,
        owner: T,
    ) {
        self.chars.push(LogicalChar {
            ch,
            display,
            logical_offset,
            owner,
        });
    }

    pub(crate) fn push_tab(&mut self, logical_offset: usize, cells: usize, owner: T) {
        self.push_char(
            '\t',
            Some(" ".repeat(cells.max(1))),
            Some(logical_offset),
            owner,
        );
    }
}

impl<T: Clone> VisualLineBuilder<T> {
    pub(crate) fn push_text(&mut self, text: &str, logical_start: usize, owner: T) {
        self.push_directed_text(text, logical_start, RunDirection::Natural, owner);
    }

    pub(crate) fn push_directed_text(
        &mut self,
        text: &str,
        logical_start: usize,
        direction: RunDirection,
        owner: T,
    ) {
        let start = self.chars.len();
        for (i, ch) in text.chars().enumerate() {
            self.push_char(ch, None, Some(logical_start + i), owner.clone());
        }
        self.push_direction(start, direction);
    }

    pub(crate) fn push_unmapped_text(&mut self, text: &str, owner: T) {
        let start = self.chars.len();
        for ch in text.chars() {
            self.push_char(ch, None, None, owner.clone());
        }
        self.push_direction(start, RunDirection::Natural);
    }

    pub(crate) fn build(self) -> VisualLine<T> {
        if self.chars.is_empty() {
            return VisualLine {
                base: self.base,
                clusters: Vec::new(),
                width: 0,
            };
        }

        let (bidi_text, source_by_bidi_char) = self.bidi_text();
        let bidi = ParagraphBidiInfo::new(&bidi_text, self.base.level());
        let levels = bidi.reordered_levels_per_char(0..bidi_text.len());
        let logical_clusters =
            logical_clusters(&bidi_text, &source_by_bidi_char, &levels, &self.chars);
        let cluster_levels: Vec<Level> = logical_clusters.iter().map(|c| c.level).collect();
        let visual_order = BidiInfo::reorder_visual(&cluster_levels);

        let mut width = 0usize;
        let mut clusters = Vec::new();
        for logical_index in visual_order {
            let raw = &logical_clusters[logical_index];
            if raw.injected_only {
                continue;
            }
            let start = width;
            width += display_width(&raw.display);
            clusters.push(VisualCluster {
                logical_index,
                text: raw.text.clone(),
                display: raw.display.clone(),
                chars: raw.chars.clone(),
                logical: raw.logical.clone(),
                level: raw.level.number(),
                cells: start..width,
            });
        }

        VisualLine {
            base: self.base,
            clusters,
            width,
        }
    }

    fn push_direction(&mut self, start: usize, direction: RunDirection) {
        let end = self.chars.len();
        if direction != RunDirection::Natural && start < end {
            self.directions.push(DirectionalRange {
                start,
                end,
                direction,
            });
        }
    }

    fn bidi_text(&self) -> (String, Vec<Option<usize>>) {
        let mut opens = vec![Vec::<char>::new(); self.chars.len() + 1];
        let mut closes = vec![0usize; self.chars.len() + 1];
        for range in &self.directions {
            if let Some(open) = range.direction.open_control() {
                opens[range.start].push(open);
                closes[range.end] += 1;
            }
        }

        let mut text = String::new();
        let mut sources = Vec::new();
        for idx in 0..=self.chars.len() {
            for _ in 0..closes[idx] {
                text.push(PDF);
                sources.push(None);
            }
            for &open in &opens[idx] {
                text.push(open);
                sources.push(None);
            }
            if idx < self.chars.len() {
                text.push(self.chars[idx].ch);
                sources.push(Some(idx));
            }
        }
        (text, sources)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClusterChar<T> {
    pub ch: char,
    pub logical_offset: Option<usize>,
    pub owner: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalCluster<T> {
    text: String,
    display: String,
    chars: Vec<ClusterChar<T>>,
    logical: Option<Range<usize>>,
    level: Level,
    injected_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualCluster<T> {
    /// Index of this cluster in logical cluster order before UBA L2 reordering.
    pub logical_index: usize,
    /// Source text for the cluster, excluding projection-injected controls.
    pub text: String,
    /// Terminal text to draw. Unicode bidi controls are kept in `text` but not
    /// in this display string; combining and ZWJ code points remain with their
    /// base grapheme.
    pub display: String,
    pub chars: Vec<ClusterChar<T>>,
    /// Logical editor offsets covered by this cluster. Generated fields,
    /// revisions, and other unmapped display text keep their owners but have no
    /// caret range.
    pub logical: Option<Range<usize>>,
    pub level: u8,
    /// Half-open terminal cell span occupied by the display string.
    pub cells: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaretEdge {
    /// Edge before the cluster in logical text order. For odd bidi levels this
    /// is the right edge of the visual cluster.
    Leading,
    /// Edge after the cluster in logical text order. For odd bidi levels this
    /// is the left edge of the visual cluster.
    Trailing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CaretCell {
    pub col: usize,
    pub edge: CaretEdge,
    pub cluster: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hit {
    pub logical_offset: usize,
    pub col: usize,
    pub edge: CaretEdge,
    pub cluster: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualLine<T> {
    pub base: BaseDirection,
    pub clusters: Vec<VisualCluster<T>>,
    pub width: usize,
}

impl<T> VisualLine<T> {
    pub(crate) fn visible_text(&self) -> String {
        self.clusters.iter().map(|c| c.display.as_str()).collect()
    }

    /// Return the requested visual edge for a logical caret offset.
    ///
    /// If the offset is between two clusters, `Leading` prefers the following
    /// logical cluster and `Trailing` prefers the preceding logical cluster.
    /// If the offset falls inside a multi-codepoint grapheme cluster, the caret
    /// snaps to that cluster's leading or trailing edge so combining sequences,
    /// controls, and emoji ZWJ sequences never expose an interior terminal cell.
    pub(crate) fn caret_for_offset(
        &self,
        logical_offset: usize,
        edge: CaretEdge,
    ) -> Option<CaretCell> {
        let mut fallback = None;
        for (cluster_idx, cluster) in self.clusters.iter().enumerate() {
            let Some(range) = &cluster.logical else {
                continue;
            };
            if range.start == logical_offset && edge == CaretEdge::Leading {
                return Some(caret_cell(cluster_idx, cluster, CaretEdge::Leading));
            }
            if range.end == logical_offset && edge == CaretEdge::Trailing {
                return Some(caret_cell(cluster_idx, cluster, CaretEdge::Trailing));
            }
            if range.start == logical_offset {
                fallback = Some(caret_cell(cluster_idx, cluster, CaretEdge::Leading));
            } else if range.end == logical_offset {
                fallback = Some(caret_cell(cluster_idx, cluster, CaretEdge::Trailing));
            } else if range.start < logical_offset && logical_offset < range.end {
                return Some(caret_cell(cluster_idx, cluster, edge));
            }
        }
        fallback
    }

    /// Map a terminal column to the nearest logical caret boundary.
    ///
    /// The input is interpreted as a terminal cell column, matching the existing
    /// renderer's nearest-boundary hit testing. Unmapped generated text and
    /// zero-width controls are not returned as editable hits, but they still
    /// influence the nearest editable boundary through their visual cell spans.
    pub(crate) fn hit_test(&self, col: usize) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let mut best_distance = usize::MAX;
        for (cluster_idx, cluster) in self.clusters.iter().enumerate() {
            let Some(range) = &cluster.logical else {
                continue;
            };
            for edge in [CaretEdge::Leading, CaretEdge::Trailing] {
                let caret = caret_cell(cluster_idx, cluster, edge);
                let distance = caret.col.abs_diff(col);
                if distance < best_distance {
                    let logical_offset = match edge {
                        CaretEdge::Leading => range.start,
                        CaretEdge::Trailing => range.end,
                    };
                    best_distance = distance;
                    best = Some(Hit {
                        logical_offset,
                        col: caret.col,
                        edge,
                        cluster: cluster_idx,
                    });
                }
            }
        }
        best
    }
}

fn caret_cell<T>(cluster_idx: usize, cluster: &VisualCluster<T>, edge: CaretEdge) -> CaretCell {
    let col = match (cluster.level % 2 == 1, edge) {
        (false, CaretEdge::Leading) | (true, CaretEdge::Trailing) => cluster.cells.start,
        (false, CaretEdge::Trailing) | (true, CaretEdge::Leading) => cluster.cells.end,
    };
    CaretCell {
        col,
        edge,
        cluster: cluster_idx,
    }
}

#[derive(Clone, Copy)]
struct BidiChar {
    byte_start: usize,
    level: Level,
    source: Option<usize>,
}

fn logical_clusters<T: Clone>(
    bidi_text: &str,
    source_by_bidi_char: &[Option<usize>],
    levels: &[Level],
    source_chars: &[LogicalChar<T>],
) -> Vec<LogicalCluster<T>> {
    let mut chars = Vec::new();
    for (char_idx, (byte_start, _)) in bidi_text.char_indices().enumerate() {
        chars.push(BidiChar {
            byte_start,
            level: levels[char_idx],
            source: source_by_bidi_char[char_idx],
        });
    }

    let mut out = Vec::new();
    let mut char_idx = 0usize;
    for (byte_start, grapheme) in bidi_text.grapheme_indices(true) {
        let byte_end = byte_start + grapheme.len();
        let first_char = char_idx;
        while char_idx < chars.len() && chars[char_idx].byte_start < byte_end {
            char_idx += 1;
        }
        let cluster_chars = &chars[first_char..char_idx];
        let level = cluster_chars
            .iter()
            .find(|c| c.source.is_some())
            .or_else(|| cluster_chars.first())
            .map(|c| c.level)
            .unwrap_or(Level::ltr());

        let mut text = String::new();
        let mut display = String::new();
        let mut owners = Vec::new();
        for ch in cluster_chars {
            if let Some(source_idx) = ch.source {
                let src = &source_chars[source_idx];
                text.push(src.ch);
                if let Some(display_override) = &src.display {
                    display.push_str(display_override);
                } else if !is_bidi_format(src.ch) {
                    display.push(src.ch);
                }
                owners.push(ClusterChar {
                    ch: src.ch,
                    logical_offset: src.logical_offset,
                    owner: src.owner.clone(),
                });
            }
        }

        let offsets: Vec<usize> = owners.iter().filter_map(|c| c.logical_offset).collect();
        let logical = if offsets.is_empty() {
            None
        } else {
            let start = offsets.iter().copied().min().unwrap();
            let end = offsets.iter().copied().max().unwrap() + 1;
            Some(start..end)
        };

        out.push(LogicalCluster {
            text,
            display,
            chars: owners,
            logical,
            level,
            injected_only: cluster_chars.iter().all(|c| c.source.is_none()),
        });
    }
    out
}

fn is_bidi_format(ch: char) -> bool {
    matches!(
        ch as u32,
        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
    )
}

fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Terminal width policy copied from the current DOCX renderer: combining marks
/// and bidi/ZWJ controls are zero-width, CJK and most emoji are two cells, and
/// all other scalar values occupy one cell.
fn char_width(c: char) -> usize {
    let u = c as u32;
    if u == 0 {
        return 0;
    }
    if (0x0300..=0x036f).contains(&u) || (0x200b..=0x200f).contains(&u) {
        return 0;
    }
    let wide = matches!(u,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe10..=0xfe19
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff
        | 0x20000..=0x3fffd
    );
    if wide { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(text: &str, base: BaseDirection) -> VisualLine<&'static str> {
        let mut builder = VisualLineBuilder::new(base);
        builder.push_text(text, 0, "body");
        builder.build()
    }

    fn starts(line: &VisualLine<&'static str>) -> Vec<usize> {
        line.clusters
            .iter()
            .filter_map(|c| c.logical.as_ref().map(|r| r.start))
            .collect()
    }

    #[test]
    fn standard_bidi_examples_reorder_logical_clusters() {
        let cases = [
            ("abc", BaseDirection::Auto, "abc", vec![0, 1, 2]),
            (
                "אבגabc",
                BaseDirection::Auto,
                "abcגבא",
                vec![3, 4, 5, 2, 1, 0],
            ),
            (
                "abc אבג",
                BaseDirection::Auto,
                "abc גבא",
                vec![0, 1, 2, 3, 6, 5, 4],
            ),
        ];

        for (logical, base, visual, visual_offsets) in cases {
            let line = project(logical, base);
            assert_eq!(line.visible_text(), visual);
            assert_eq!(starts(&line), visual_offsets);
        }
    }

    #[test]
    fn run_override_reorders_without_losing_owner_or_offsets() {
        let mut builder = VisualLineBuilder::new(BaseDirection::Ltr);
        builder.push_text("A", 0, "latin");
        builder.push_directed_text("abc", 1, RunDirection::RtlOverride, "override");
        builder.push_text("Z", 4, "latin");

        let line = builder.build();

        assert_eq!(line.visible_text(), "AcbaZ");
        let owners: Vec<_> = line
            .clusters
            .iter()
            .flat_map(|c| c.chars.iter().map(|ch| ch.owner))
            .collect();
        assert_eq!(
            owners,
            vec!["latin", "override", "override", "override", "latin"]
        );
        assert_eq!(starts(&line), vec![0, 3, 2, 1, 4]);
    }

    #[test]
    fn generated_field_like_text_keeps_owner_but_is_not_editable() {
        let mut builder = VisualLineBuilder::new(BaseDirection::Ltr);
        builder.push_text("A", 0, "editable");
        builder.push_unmapped_text("אב", "field");
        builder.push_text("Z", 1, "editable");

        let line = builder.build();

        assert_eq!(line.visible_text(), "AבאZ");
        assert_eq!(
            line.clusters
                .iter()
                .flat_map(|c| c.chars.iter().map(|ch| ch.owner))
                .collect::<Vec<_>>(),
            vec!["editable", "field", "field", "editable"]
        );
        assert_eq!(line.hit_test(2).map(|hit| hit.logical_offset), Some(1));
    }

    #[test]
    fn combining_marks_and_emoji_sequences_are_single_caret_clusters() {
        let line = project("a\u{0301}👩\u{200d}💻b", BaseDirection::Ltr);

        assert_eq!(line.visible_text(), "a\u{0301}👩\u{200d}💻b");
        assert_eq!(line.clusters.len(), 3);
        assert_eq!(line.clusters[0].logical, Some(0..2));
        assert_eq!(line.clusters[0].cells, 0..1);
        assert_eq!(line.clusters[1].logical, Some(2..5));
        assert_eq!(line.clusters[1].cells, 1..5);
        assert_eq!(
            line.caret_for_offset(3, CaretEdge::Leading).map(|c| c.col),
            Some(1)
        );
        assert_eq!(
            line.caret_for_offset(3, CaretEdge::Trailing).map(|c| c.col),
            Some(5)
        );
    }

    #[test]
    fn tabs_and_wide_chars_have_stable_cell_spans() {
        let mut builder = VisualLineBuilder::new(BaseDirection::Ltr);
        builder.push_text("a", 0, "body");
        builder.push_tab(1, 4, "tab");
        builder.push_text("哈b", 2, "body");
        let line = builder.build();

        assert_eq!(line.visible_text(), "a    哈b");
        assert_eq!(line.clusters[0].cells, 0..1);
        assert_eq!(line.clusters[1].logical, Some(1..2));
        assert_eq!(line.clusters[1].cells, 1..5);
        assert_eq!(line.clusters[2].cells, 5..7);
        assert_eq!(line.clusters[3].cells, 7..8);
        assert_eq!(line.hit_test(6).map(|hit| hit.logical_offset), Some(2));
    }

    #[test]
    fn explicit_unicode_controls_are_zero_width_and_hidden() {
        let line = project("a\u{202e}bc\u{202c}d", BaseDirection::Ltr);

        assert_eq!(line.visible_text(), "acbd");
        assert_eq!(line.width, 4);
        assert!(line.clusters.iter().any(|c| c.text == "\u{202e}"));
        let control = line.clusters.iter().find(|c| c.text == "\u{202e}").unwrap();
        assert_eq!(control.display, "");
        assert_eq!(control.cells.start, control.cells.end);
        assert_eq!(
            line.caret_for_offset(1, CaretEdge::Leading).map(|c| c.col),
            Some(1)
        );
    }

    #[test]
    fn rtl_cluster_edges_follow_logical_leading_and_trailing() {
        let line = project("אב", BaseDirection::Rtl);

        assert_eq!(line.visible_text(), "בא");
        let alef = line
            .clusters
            .iter()
            .find(|c| c.logical == Some(0..1))
            .unwrap();
        assert_eq!(alef.cells, 1..2);
        assert_eq!(
            line.caret_for_offset(0, CaretEdge::Leading).map(|c| c.col),
            Some(2)
        );
        assert_eq!(
            line.caret_for_offset(1, CaretEdge::Trailing).map(|c| c.col),
            Some(1)
        );
    }
}
