//! The page-view pagination (#745): each section flows with its own page
//! size, margins, gutter and columns, and starts as its `w:type` says —
//! agreeing with the PDF exporter (docxcore/src/export.rs), which is the
//! oracle these fixtures mirror.

use super::*;
use crate::hf::tests::{body_blocks, para};
use docxcore::model::Inline;

/// A sectPr with the given geometry; absent `w:pgMar`/`w:cols` children take
/// Word's defaults via `SectionSetup::parse`.
fn geom_sect(
    w: i32,
    h: i32,
    top: i32,
    right: i32,
    bottom: i32,
    left: i32,
    gutter: i32,
    extra: &str,
) -> String {
    format!(
        "<w:sectPr><w:pgSz w:w=\"{w}\" w:h=\"{h}\"/>\
         <w:pgMar w:top=\"{top}\" w:right=\"{right}\" w:bottom=\"{bottom}\" w:left=\"{left}\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"{gutter}\"/>{extra}</w:sectPr>"
    )
}

/// Letter portrait, 1" margins, no gutter, with `extra` children.
fn letter(extra: &str) -> String {
    geom_sect(12240, 15840, 1440, 1440, 1440, 1440, 0, extra)
}

#[test]
fn landscape_middle_section_gets_its_own_sheet_size() {
    let body = body_blocks(&format!(
        "{}{}{}",
        para("One", Some(&letter(""))),
        para(
            "Two",
            Some(&geom_sect(15840, 12240, 1440, 1440, 1440, 1440, 0, ""))
        ),
        letter("")
    ));
    let trailing = letter("");
    let pf = flow(&body, &trailing, false);
    assert_eq!(pf.pages.len(), 3, "{:?}", pf.ranges());
    assert_eq!((pf.sections[0].w, pf.sections[0].h), (12240, 15840));
    assert_eq!((pf.sections[1].w, pf.sections[1].h), (15840, 12240));
    assert_eq!((pf.sections[2].w, pf.sections[2].h), (12240, 15840));
    // Each page is owned by its own section (what hf slots and distances use).
    let owners: Vec<usize> = pf.pages.iter().map(|p| p.bands[0].section).collect();
    assert_eq!(owners, vec![0, 1, 2]);
}

#[test]
fn margins_and_gutter_are_per_section() {
    let sect = geom_sect(12240, 15840, 2880, 2160, 2880, 2160, 720, "");
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    let sb = &pf.sections[0];
    assert_eq!(
        (sb.top, sb.right, sb.bottom, sb.left),
        (2880, 2160, 2880, 2880)
    );
}

#[test]
fn gutter_at_top_adds_to_top_margin() {
    let sect = geom_sect(12240, 15840, 2880, 2160, 2880, 2160, 720, "");
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, true);
    let sb = &pf.sections[0];
    // The gutter widens the top, not the left (export.rs `top_margin`).
    assert_eq!((sb.top, sb.left), (3600, 2160));
}

#[test]
fn columns_are_per_section() {
    let two = letter(r#"<w:cols w:num="2"/>"#);
    let three = letter(r#"<w:cols w:num="3" w:space="360"/>"#);
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&two)),
        para("Two", Some(&three))
    ));
    let pf = flow(&body, &three, false);
    assert_eq!(pf.sections[0].col_w, vec![4320, 4320]);
    assert_eq!(pf.sections[0].col_gap, vec![720, 720]);
    assert_eq!(pf.sections[1].col_w, vec![2880, 2880, 2880]);
    assert_eq!(pf.sections[1].col_gap, vec![360, 360, 360]);
}

#[test]
fn unequal_columns_use_their_widths() {
    let sect = letter(
        r#"<w:cols w:num="2" w:equalWidth="0"><w:col w:w="5000" w:space="600"/><w:col w:w="4000"/></w:cols>"#,
    );
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    assert_eq!(pf.sections[0].col_w, vec![5000, 4000]);
    assert_eq!(pf.sections[0].col_gap, vec![600, 0]);
}

#[test]
fn sep_flag_is_carried() {
    let sect = letter(r#"<w:cols w:num="2" w:sep="1"/>"#);
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    assert!(pf.sections[0].sep);
}

#[test]
fn continuous_same_size_shares_the_page_with_two_bands() {
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&letter(""))),
        para("Two", Some(&letter(r#"<w:type w:val="continuous"/>"#)))
    ));
    let pf = flow(&body, "", false);
    assert_eq!(pf.pages.len(), 1, "{:?}", pf.ranges());
    assert_eq!(pf.pages[0].bands.len(), 2);
    assert_eq!(pf.pages[0].bands[0].section, 0);
    assert_eq!(pf.pages[0].bands[0].cols, vec![(0, 1)]);
    assert_eq!(pf.pages[0].bands[1].section, 1);
    assert_eq!(pf.pages[0].bands[1].cols, vec![(1, 2)]);
}

#[test]
fn continuous_with_other_size_starts_a_page() {
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&letter(""))),
        para(
            "Two",
            Some(&geom_sect(
                15840,
                12240,
                1440,
                1440,
                1440,
                1440,
                0,
                r#"<w:type w:val="continuous"/>"#
            ))
        )
    ));
    let pf = flow(&body, "", false);
    assert_eq!(pf.pages.len(), 2, "{:?}", pf.ranges());
    assert_eq!(pf.pages[0].bands[0].section, 0);
    assert_eq!(pf.pages[1].bands[0].section, 1);
    assert_eq!((pf.sections[1].w, pf.sections[1].h), (15840, 12240));
}

#[test]
fn next_column_acts_as_continuous() {
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&letter(""))),
        para("Two", Some(&letter(r#"<w:type w:val="nextColumn"/>"#)))
    ));
    let pf = flow(&body, "", false);
    assert_eq!(pf.pages.len(), 1, "{:?}", pf.ranges());
    assert_eq!(pf.pages[0].bands.len(), 2);
}

#[test]
fn continuous_after_two_columns_balances_them() {
    let two_col = letter(r#"<w:cols w:num="2"/>"#);
    let body = body_blocks(&format!(
        "{}{}{}{}{}",
        para("1", None),
        para("2", None),
        para("3", None),
        para("4", Some(&two_col)),
        para("5", Some(&letter(r#"<w:type w:val="continuous"/>"#)))
    ));
    let pf = flow(&body, "", false);
    assert_eq!(pf.pages.len(), 1, "{:?}", pf.ranges());
    // The two-column band is balanced 2+2 before the continuous band below.
    assert_eq!(pf.pages[0].bands[0].cols, vec![(0, 2), (2, 4)]);
    assert_eq!(pf.pages[0].bands[1].cols, vec![(4, 5)]);
}

#[test]
fn odd_page_inserts_a_blank_page_when_needed() {
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&letter(""))),
        letter(r#"<w:type w:val="oddPage"/>"#)
    ));
    let trailing = letter(r#"<w:type w:val="oddPage"/>"#);
    let pf = flow(&body, &trailing, false);
    assert_eq!(pf.pages.len(), 3, "{:?}", pf.ranges());
    // Page 2 (index 1) is the blank filler: an empty band, previous
    // section's size; it holds no block but has a range start.
    assert_eq!(pf.pages[1].bands[0].section, 0);
    assert_eq!(pf.pages[1].bands[0].cols, vec![(1, 1)]);
    assert_eq!(pf.ranges()[1], vec![(1, 1)]);
    // The odd section's own page holds the trailing section block.
    assert_eq!(pf.ranges()[2], vec![(1, 2)]);
    assert_eq!(crate::page_of_block(&pf.ranges(), 0), Some(0));
}

#[test]
fn even_page_starts_on_the_next_even_page() {
    let trailing = letter(r#"<w:type w:val="evenPage"/>"#);
    let body = body_blocks(&format!("{}{}", para("One", Some(&letter(""))), trailing));
    let pf = flow(&body, &trailing, false);
    assert_eq!(pf.pages.len(), 2, "{:?}", pf.ranges());
    assert_eq!(pf.ranges()[0], vec![(0, 1)]);
    assert_eq!(pf.ranges()[1], vec![(1, 2)]);
}

#[test]
fn pgnumtype_start_changes_parity() {
    // One page so far (number 1, odd): an oddPage section with start=3 needs
    // no filler; with start=4 the parity mismatches and a blank page comes
    // first, like export.rs `start_section`.
    let odd3 = letter(r#"<w:type w:val="oddPage"/><w:pgNumType w:start="3"/>"#);
    let body = body_blocks(&format!("{}{}", para("One", Some(&letter(""))), odd3));
    let pf = flow(&body, &odd3, false);
    assert_eq!(pf.pages.len(), 2, "{:?}", pf.ranges());
    let odd4 = letter(r#"<w:type w:val="oddPage"/><w:pgNumType w:start="4"/>"#);
    let body = body_blocks(&format!("{}{}", para("One", Some(&letter(""))), odd4));
    let pf = flow(&body, &odd4, false);
    assert_eq!(pf.pages.len(), 3, "{:?}", pf.ranges());
    assert_eq!(pf.pages[1].bands[0].cols, vec![(1, 1)]);
}

#[test]
fn changing_the_final_section_leaves_earlier_pages_alone() {
    let mid = geom_sect(15840, 12240, 2160, 2160, 2160, 2160, 360, "");
    let body = body_blocks(&format!(
        "{}{}",
        para("One", Some(&letter(""))),
        para("Two", Some(&mid))
    ));
    let portrait = letter("");
    let landscape = geom_sect(15840, 12240, 720, 720, 720, 720, 0, "");
    let pf1 = flow(&body, &portrait, false);
    let pf2 = flow(&body, &landscape, false);
    assert_eq!(pf1.sections[0], pf2.sections[0]);
    assert_eq!(pf1.pages[0], pf2.pages[0]);
    assert_eq!(pf1.sections[2].w, 12240);
    assert_eq!(pf2.sections[2].w, 15840);
}

#[test]
fn ranges_cover_every_block_once() {
    let mut xml = String::new();
    for i in 0..300 {
        let sect = match i {
            99 => Some(letter("")),
            199 => Some(letter(r#"<w:cols w:num="2"/>"#)),
            _ => None,
        };
        xml.push_str(&para(&format!("p{i}"), sect.as_deref()));
    }
    let body = body_blocks(&xml);
    let trailing = letter("");
    let pf = flow(&body, &trailing, false);
    assert!(pf.pages.len() > 1, "{:?}", pf.ranges());
    let mut covered: Vec<usize> = Vec::new();
    for page in &pf.ranges() {
        for &(s, e) in page {
            assert!(s <= e, "({s}, {e})");
            covered.extend(s..e);
        }
    }
    assert_eq!(covered, (0..300).collect::<Vec<_>>(), "gap or overlap");
    // page_of_block agrees with the ranges.
    for b in 0..300 {
        let expect = pf
            .ranges()
            .iter()
            .position(|cols| cols.iter().any(|&(s, e)| s <= b && b < e));
        assert_eq!(crate::page_of_block(&pf.ranges(), b), expect);
    }
}

#[test]
fn page_break_ends_the_page() {
    // A hard page break mid-section starts a new page for the rest, as
    // `paginate` did (the cover_page test migration keeps this assertion).
    let br = |text: &str| {
        Block::Paragraph(docxcore::model::Paragraph {
            content: vec![
                Inline::Run(docxcore::model::Run {
                    text: text.into(),
                    ..Default::default()
                }),
                Inline::Break(docxcore::model::BreakKind::Page, Default::default()),
            ],
            ..Default::default()
        })
    };
    let body = vec![
        br("One"),
        br("Two"),
        Block::Paragraph(docxcore::model::Paragraph {
            content: vec![Inline::Run(docxcore::model::Run {
                text: "Three".into(),
                ..Default::default()
            })],
            ..Default::default()
        }),
    ];
    let pf = flow(&body, &letter(""), false);
    assert_eq!(pf.ranges()[0], vec![(0, 1)]);
    assert_eq!(pf.ranges()[1], vec![(1, 2)]);
    assert_eq!(pf.ranges()[2], vec![(2, 3)]);
    assert_eq!(pf.pages.len(), 3, "{:?}", pf.ranges());
}
