//! Print Layout line numbers (`w:lnNumType`) for the suite page view (#746):
//! the counting logic, kept free of gpui so it is unit-testable. The PDF
//! exporter (`docxcore/src/export.rs` `LineNumbers::from_setup` /
//! `Region::count_line`) is the oracle; every counting scenario below is a
//! port of one of its tests.

use docxcore::sect::LnRestart;

/// `w:lnNumType` resolved for drawing, mirroring export.rs `LineNumbers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rule {
    pub count_by: u32,
    /// Word writes the dialog's "Start at" minus one, so the first line is
    /// numbered `start + 1` (export.rs `LineNumbers::start`).
    pub start: u32,
    /// Number right edge → text column, twips; absent/zero is Word's
    /// "Auto", a quarter inch (export.rs defaults to 360).
    pub distance_tw: i32,
    pub restart: LnRestart,
}

impl Rule {
    pub(crate) fn from_setup(ln: docxcore::sect::LineNumbering) -> Rule {
        Rule {
            count_by: ln.count_by.max(1) as u32,
            start: ln.start.unwrap_or(0).max(0) as u32,
            distance_tw: ln.distance.filter(|&d| d > 0).unwrap_or(360),
            restart: ln.restart,
        }
    }
}

/// One numbered paragraph, in document (paint) order: the page view pushes
/// one per numbered top-level paragraph each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub page: usize,
    pub section: usize,
    pub rule: Rule,
}

/// The counter value before each entry's first line, accumulated exactly as
/// export.rs `count_line` does. An entry with `rows[k] == 0` neither
/// restarts nor records a "last" page/section — page-view-only behaviour,
/// a hedge for a canvas that has not painted yet (not an export.rs
/// scenario: export counts every placed line, and an empty paragraph is one
/// line). NOTE: a row whose painted height reads as several `line_h`s (a
/// mixed larger inline font) is counted as that many rows, so later numbers
/// can run ahead of the PDF until the next restart (known limitation).
pub(crate) fn first_counts(entries: &[Entry], rows: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(entries.len());
    let mut count = 0u32;
    let mut last: Option<(usize, usize)> = None;
    for (k, e) in entries.iter().enumerate() {
        let r = rows.get(k).copied().unwrap_or(0);
        if r == 0 {
            out.push(count);
            continue;
        }
        let restart = match (last, e.rule.restart) {
            (None, _) => true,
            (Some((p, _)), LnRestart::NewPage) => p != e.page,
            (Some((_, s)), LnRestart::NewSection) => s != e.section,
            (Some(_), LnRestart::Continuous) => false,
        };
        if restart {
            count = e.rule.start;
        }
        out.push(count);
        count += r;
        last = Some((e.page, e.section));
    }
    out
}

/// Visual rows of a painted wrapping row: its height over the line height.
/// A single-line row sits at `min_h` (main.rs `paragraph_el`'s
/// `line_h.max(base + 6.)`), which is taller than `line_h`, so
/// at-or-under `min_h + 0.5` means one row. A dead `line_h` means "cannot
/// tell": one. KNOWN LIMITATION: rows are counted from painted height
/// alone, so two (or more) tight lines (a small line-spacing multiple)
/// whose combined height stays under `min_h` are indistinguishable from
/// one line at `min_h` and read as one row — numbering after such a
/// paragraph can run one short until the next restart. Laying the row out
/// without `min_h` would change pagination, and gpui reports no wrapped
/// line count, so there is nothing safe to count instead.
pub(crate) fn row_count(height: f32, line_h: f32, min_h: f32) -> u32 {
    if height <= min_h + 0.5 || line_h <= 0.0 {
        return 1;
    }
    (height / line_h).round().max(1.0) as u32
}

/// Whether a top-level body paragraph is numbered: its section sets
/// `w:lnNumType` and neither its own props nor its style chain suppresses
/// line numbers (export.rs `emit_paragraph`'s gate). Tables and
/// header/footer paragraphs never reach this call.
pub(crate) fn numbered_rule(
    p: &docxcore::model::Paragraph,
    styles: &docxcore::styles::StyleSheet,
    sect: Option<docxcore::sect::LineNumbering>,
) -> Option<Rule> {
    let ln = sect?;
    (!styles.effective_ppr_flag(
        p.props.style_id.as_deref(),
        &p.props,
        docxcore::styles::PprFlag::SuppressLineNumbers,
    ))
    .then(|| Rule::from_setup(ln))
}

/// The frame's line-number state, shared by the numbered rows' canvases in
/// paint order — numbering depends on it, so the render rebuilds `entries`
/// and `rows` (document order) before the canvases paint and each canvas
/// records its own row count before reading earlier ones. `painted` is
/// only the harness's record of what was drawn (`doc {}`). `page` is the
/// 0-based sheet index.
#[derive(Debug, Clone, Default)]
pub(crate) struct LineProbe {
    pub entries: Vec<Entry>,
    pub rows: Vec<u32>,
    pub painted: Vec<Painted>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Painted {
    pub page: usize,
    pub n: u32,
    pub x: f32,
    pub y: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Block, ParProps, Paragraph, RunProps};
    use docxcore::sect::{LineNumbering, LnRestart};

    fn rule(count_by: u32, start: u32, restart: LnRestart) -> Rule {
        Rule {
            count_by,
            start,
            distance_tw: 360,
            restart,
        }
    }

    fn entry(page: usize, section: usize, r: Rule) -> Entry {
        Entry {
            page,
            section,
            rule: r,
        }
    }

    /// The drawn numbers across all entries, in order: line j (0-based) of
    /// entry k is numbered `first[k] + j + 1` and drawn iff that is a
    /// multiple of the entry's countBy (export.rs `count_line`).
    fn drawn(entries: &[Entry], rows: &[u32]) -> Vec<u32> {
        let first = first_counts(entries, rows);
        let mut out = Vec::new();
        for (k, e) in entries.iter().enumerate() {
            for j in 0..rows[k] {
                let n = first[k] + j + 1;
                if n.is_multiple_of(e.rule.count_by) {
                    out.push(n);
                }
            }
        }
        out
    }

    fn text_para(text: &str) -> Paragraph {
        Paragraph {
            props: ParProps::default(),
            content: vec![docxcore::model::Inline::Run(docxcore::model::Run {
                text: text.to_string(),
                props: RunProps::default(),
            })],
        }
    }

    /// Port of export.rs `line_numbers_count_by_start_and_restart`.
    #[test]
    fn count_by_start_and_default_start() {
        let r = rule(1, 0, LnRestart::NewPage);
        let entries = [entry(0, 0, r), entry(0, 0, r), entry(0, 0, r)];
        assert_eq!(drawn(&entries, &[1, 1, 1]), [1, 2, 3]);

        // countBy 5, start 4 (Word's "Start at: 5"): lines are numbered
        // 5..=16, so 5, 10 and 15 are drawn.
        let r = rule(5, 4, LnRestart::NewPage);
        let entries = [entry(0, 0, r)];
        assert_eq!(drawn(&entries, &[12]), [5, 10, 15]);
    }

    /// Port of the `two_pages` half of export.rs
    /// `line_numbers_count_by_start_and_restart`.
    #[test]
    fn new_page_restarts_continuous_does_not() {
        // newPage (the default) restarts on each page.
        let r = rule(1, 0, LnRestart::NewPage);
        let entries = [entry(0, 0, r), entry(1, 0, r)];
        assert_eq!(drawn(&entries, &[1, 1]), [1, 1]);

        // continuous never restarts.
        let r = rule(1, 0, LnRestart::Continuous);
        let entries = [entry(0, 0, r), entry(1, 0, r)];
        assert_eq!(drawn(&entries, &[1, 1]), [1, 2]);
    }

    /// Port of export.rs `line_numbers_restart_new_section_continuous_break`.
    #[test]
    fn new_section_restarts_on_section_not_page() {
        // A paragraph-level sectPr closes the section the paragraph itself
        // is in, so a0 and a1 are both section 0 and b0 is section 1.
        let r = rule(1, 0, LnRestart::NewSection);
        let entries = [entry(0, 0, r), entry(0, 0, r), entry(0, 1, r)];
        assert_eq!(drawn(&entries, &[1, 1, 1]), [1, 2, 1]);

        // A new page does not restart newSection numbering.
        let entries = [entry(0, 0, r), entry(1, 0, r)];
        assert_eq!(drawn(&entries, &[1, 1]), [1, 2]);
    }

    /// PAGE-VIEW-ONLY behaviour, not an oracle port (export.rs has no
    /// zero-line paragraphs: an empty paragraph counts as one row). A canvas
    /// that has not painted yet still has rows[k] == 0; such an entry must
    /// not restart the counter (as if the run began at the next painted
    /// paragraph) nor record a "last" page/section.
    #[test]
    fn a_zero_row_entry_neither_restarts_nor_counts() {
        let r = rule(1, 4, LnRestart::Continuous);
        let entries = [entry(3, 0, r), entry(3, 0, r)];
        // The unpainted first entry is skipped, so the second restarts at
        // start = 4 and its first line is numbered 5.
        assert_eq!(first_counts(&entries, &[0, 1]), [0, 4]);
        assert_eq!(drawn(&entries, &[0, 1]), [5]);
    }

    /// Side effect of counting rows, port of export.rs's multi-line
    /// paragraph cases: a 3-row paragraph shifts the next paragraph's first
    /// number by 3.
    #[test]
    fn multi_line_paragraph_counts_every_row() {
        let r = rule(1, 0, LnRestart::Continuous);
        let entries = [entry(0, 0, r), entry(0, 0, r)];
        assert_eq!(drawn(&entries, &[3, 1]), [1, 2, 3, 4]);
        assert_eq!(first_counts(&entries, &[3, 1]), [0, 3]);
    }

    #[test]
    fn row_count_single_line_and_wrapped() {
        // One line at the min height, three wrapped lines, a degenerate
        // 10px height (still one line), and a dead line_h.
        assert_eq!(row_count(20.5, 19.6, 20.5), 1);
        assert_eq!(row_count(59.0, 19.6, 20.5), 3);
        assert_eq!(row_count(10.0, 9.8, 20.5), 1);
        assert_eq!(row_count(40.0, 0.0, 20.5), 1);
    }

    /// gpui draws each wrapped line at the pixel-snapped line height
    /// (`pixel_snap`): 24 lines of a 19.575px font are 480px at 20px per
    /// line. The paint closure passes the snapped pitch; the raw 19.575
    /// would divide 480 into 25 and number a line that is not there.
    #[test]
    fn row_count_counts_at_the_snapped_line_height() {
        assert_eq!(row_count(480.0, 20.0, 20.5), 24);
        assert_eq!((480.0f32 / 19.575).round().max(1.0) as u32, 25);
    }

    /// Port of export.rs `LineNumbers::from_setup`'s distance rule.
    #[test]
    fn distance_defaults_to_360_when_absent_or_zero() {
        let mk = |distance: Option<i32>| LineNumbering {
            count_by: 1,
            start: None,
            distance,
            restart: LnRestart::NewPage,
        };
        assert_eq!(Rule::from_setup(mk(None)).distance_tw, 360);
        assert_eq!(Rule::from_setup(mk(Some(0))).distance_tw, 360);
        assert_eq!(Rule::from_setup(mk(Some(720))).distance_tw, 720);
        // countBy and start clamp like export.rs.
        assert_eq!(Rule::from_setup(mk(None)).count_by, 1);
        assert_eq!(
            Rule::from_setup(LineNumbering {
                count_by: 0,
                start: Some(-3),
                distance: Some(720),
                restart: LnRestart::NewPage,
            })
            .count_by,
            1
        );
        assert_eq!(
            Rule::from_setup(LineNumbering {
                count_by: 0,
                start: Some(-3),
                distance: Some(720),
                restart: LnRestart::NewPage,
            })
            .start,
            0
        );
    }

    /// Port of export.rs `suppressed_paragraphs_and_tables_...`'s gate: the
    /// numbered decision is `effective_ppr_flag` (direct wins, then the
    /// style chain). Tables never reach this: only top-level paragraphs are
    /// asked.
    #[test]
    fn suppress_gate_is_effective_ppr_flag() {
        let styles = docxcore::styles::parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:styleId="NoNum"><w:pPr><w:suppressLineNumbers/></w:pPr></w:style></w:styles>"#,
        );
        let sect = Some(LineNumbering {
            count_by: 1,
            start: None,
            distance: None,
            restart: LnRestart::Continuous,
        });
        let mut direct = text_para("direct");
        direct.props.raw_props = vec!["<w:suppressLineNumbers/>".to_string()];
        let mut styled = text_para("styled");
        styled.props.style_id = Some("NoNum".to_string());
        let mut off_style = text_para("off");
        off_style.props.style_id = Some("NoNum".to_string());
        off_style.props.raw_props = vec!["<w:suppressLineNumbers w:val=\"0\"/>".to_string()];
        let cases: Vec<Block> = vec![
            Block::Paragraph(text_para("one")),
            Block::Paragraph(direct),
            Block::Paragraph(styled),
            Block::Paragraph(off_style),
            Block::Paragraph(text_para("two")),
        ];
        let rules: Vec<Option<Rule>> = cases
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => numbered_rule(p, &styles, sect),
                _ => None,
            })
            .collect();
        // "one" and "two" number; direct and style suppress do not; a direct
        // explicit-off beats the style (Word's toggle semantics).
        assert!(rules[0].is_some());
        assert!(rules[1].is_none());
        assert!(rules[2].is_none());
        assert!(rules[3].is_some(), "direct off beats the style's on");
        assert!(rules[4].is_some());
        // Without a section rule nothing is numbered.
        assert!(numbered_rule(&text_para("one"), &styles, None).is_none());
    }
}
