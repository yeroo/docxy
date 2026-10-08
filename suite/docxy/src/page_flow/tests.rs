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

#[test]
fn balance_band_pours_at_most_ncols_columns() {
    // [10,30,10] in two columns: the smallest height the pour fits at is 40,
    // giving columns [10,30] and [10] (export.rs balance_region semantics).
    // The old fixed-target greedy produced three one-block ranges here.
    let (cols, height) = balance_band(&[10.0, 30.0, 10.0], (0, 3), 2).unwrap();
    assert_eq!(cols, vec![(0, 2), (2, 3)]);
    assert_eq!(height, 40.0);
    // Three equal blocks balance into exactly two ranges.
    let (cols, _) = balance_band(&[24.0, 24.0, 24.0], (0, 3), 2).unwrap();
    assert_eq!(cols.len(), 2);
}

#[test]
fn bands_never_have_more_ranges_than_the_section_has_columns() {
    // The 2-col section ends with a continuous follower, so its band is
    // balanced: the equal case balances 3 paragraphs 2+1, the uneven case
    // keeps the filled columns. The invariant holds for every band, and the
    // following band starts right after the balanced one.
    let two_col_cont = letter(r#"<w:cols w:num="2"/><w:type w:val="continuous"/>"#);
    let cont = letter(r#"<w:type w:val="continuous"/>"#);
    for third in ["p2", &"x".repeat(160)] {
        let body = body_blocks(&format!(
            "{}{}{}{}{}",
            para("p0", None),
            para("p1", None),
            para(third, Some(&two_col_cont)),
            para("after", None),
            cont
        ));
        let pf = flow(&body, &cont, false);
        assert_eq!(pf.pages.len(), 1, "{:?}", pf.ranges());
        for band in &pf.pages[0].bands {
            let ncols = pf.sections[band.section].col_w.len();
            assert!(
                band.cols.len() <= ncols,
                "band of section {} has {} ranges for {ncols} columns",
                band.section,
                band.cols.len()
            );
        }
        assert_eq!(
            pf.pages[0].bands[0].cols.len(),
            2,
            "the two-column band balances: {:?}",
            pf.pages[0].bands[0].cols
        );
        assert_eq!(pf.pages[0].bands[1].cols[0].0, 3, "next band follows");
    }
}

#[test]
fn continuous_band_that_does_not_fit_moves_to_a_new_page() {
    // 35 one-line paragraphs fill the sheet (35 * 24.3px of 864px); the
    // continuous section's first block must move to a new page instead of
    // overflowing under them, and no page may hold only the trailing
    // sectPr's zero-height block.
    let mut xml = String::new();
    for i in 0..35 {
        let sect = (i == 34).then(|| letter(""));
        xml.push_str(&para(&format!("p{i}"), sect.as_deref()));
    }
    xml.push_str(&para("Y", None));
    let cont = letter(r#"<w:type w:val="continuous"/>"#);
    xml.push_str(&cont);
    let body = body_blocks(&xml);
    let pf = flow(&body, &cont, false);
    assert_eq!(pf.ranges(), vec![vec![(0, 35)], vec![(35, 37)]],);
    assert_eq!(pf.pages[0].bands.len(), 1, "no empty band left on page 0");
}

#[test]
fn filler_pages_are_flagged_and_never_take_first() {
    // Section 1 is continuous with titlePg on page 1; section 2 starts on an
    // odd page, so a filler is inserted. It must be flagged, and demoted
    // from a First variant (hf::page_slots sees it as section 1's first
    // page) to Default, or Even under Different Odd & Even.
    let cont_title = letter(r#"<w:type w:val="continuous"/><w:titlePg/>"#);
    let body = body_blocks(&format!(
        "{}{}{}",
        para("One", Some(&letter(""))),
        para("Two", Some(&cont_title)),
        letter(r#"<w:type w:val="oddPage"/>"#)
    ));
    let odd = letter(r#"<w:type w:val="oddPage"/>"#);
    let pf = flow(&body, &odd, false);
    assert_eq!(pf.pages.len(), 3, "{:?}", pf.ranges());
    let flags: Vec<bool> = pf.pages.iter().map(|p| p.filler).collect();
    assert_eq!(flags, vec![false, true, false]);

    use docxcore::package::HeaderVariant;
    let slot = |variant| crate::hf::PageSlot {
        section: 1,
        variant,
    };
    let mut slots = vec![
        slot(HeaderVariant::Default),
        slot(HeaderVariant::First),
        slot(HeaderVariant::Default),
    ];
    crate::hf::demote_filler_firsts(&mut slots, &flags, false);
    assert_eq!(slots[1].variant, HeaderVariant::Default);
    let mut slots = vec![
        slot(HeaderVariant::Default),
        slot(HeaderVariant::First),
        slot(HeaderVariant::Default),
    ];
    crate::hf::demote_filler_firsts(&mut slots, &flags, true);
    assert_eq!(
        slots[1].variant,
        HeaderVariant::Even,
        "physical page 2 is even"
    );
}

#[test]
fn first_blocks_uses_the_previous_block_for_an_empty_range() {
    let ranges = vec![vec![(0, 1)], vec![(1, 1)], vec![(1, 2)]];
    assert_eq!(first_blocks(&ranges), vec![0, 0, 1]);
    assert_eq!(first_blocks(&[]), Vec::<usize>::new());
}

#[test]
fn drag_geom_keeps_the_gutter_out_of_the_margins() {
    // The ruler's margin drag writes w:left/w:top back; the geometry it
    // starts from must hold the raw pgMar values (no gutter baked in, no
    // abs), or every drag double-counts the gutter.
    let sect = geom_sect(12240, 15840, 1440, 1440, 1440, 1440, 720, "");
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    assert_eq!(
        pf.sections[0].left, 2160,
        "the drawn margin holds the gutter"
    );
    let (_, geom) = pf.drag_geom(Some(0), &sect).unwrap();
    assert_eq!(geom.ml, 1440, "the drag starts from the raw w:left");
    assert_eq!((geom.w, geom.h), (12240, 15840));
    // A left-margin drag then only adds its delta to w:left (360 twips = 24px
    // at zoom 1), and the guide tracks the drawn content edge: the content
    // starts after the gutter-inclusive margin, so the guide sits at
    // content_x + the drag delta, not at the raw margin width.
    let drag = crate::RulerDrag {
        handle: crate::RulerHandle::MarginLeft,
        start_x: 0.0,
        zoom: 1.0,
        indent: Default::default(),
        page: geom,
        content_x: 144.0, // 2160 gutter-inclusive twips at zoom 1
        content_right: 0.0,
        page_right: 0.0,
        sect_checkpointed: false,
    };
    let moved = crate::ruler_drag_result(drag, 24.0);
    match moved.change {
        crate::RulerChange::Margins { left, .. } => assert_eq!(left, 1440 + 360),
        other => panic!("expected a margins change, got {other:?}"),
    }
    assert_eq!(moved.guide, 144.0 + 24.0);
    // gutterAtTop: the top margin the drag writes is the raw w:top.
    let pf = flow(&body, &sect, true);
    let (_, geom) = pf.drag_geom(Some(0), &sect).unwrap();
    assert_eq!(geom.mt, 1440);
}

#[test]
fn two_column_section_with_few_blocks_keeps_its_column_count() {
    // Content that fits column 0 occupies one range; the renderer pads the
    // missing columns so the band still draws at the section's col_w.
    let two_col = letter(r#"<w:cols w:num="2"/>"#);
    let body = body_blocks(&para("One", Some(&two_col)));
    let pf = flow(&body, &two_col, false);
    assert_eq!(pf.sections[0].col_w.len(), 2);
    assert_eq!(pf.pages[0].bands[0].cols, vec![(0, 1)]);
}

#[test]
fn later_column_first_block_overruns_move_to_a_new_page() {
    // 32 one-line paragraphs fill ~778px of the 864px sheet; the continuous
    // two-column section's A fits column 0, and B (180 chars ~ 105.5px) must
    // not start an empty column 1 that overruns the sheet: the band keeps
    // its filled columns and B continues on a new page, with no page holding
    // only the trailing sectPr block.
    let big = "B".repeat(180);
    let two_col_cont = letter(r#"<w:cols w:num="2"/><w:type w:val="continuous"/>"#);
    let mut xml = String::new();
    for i in 0..32 {
        let sect = (i == 31).then(|| letter(""));
        xml.push_str(&para(&format!("p{i}"), sect.as_deref()));
    }
    xml.push_str(&para("A", None));
    xml.push_str(&para(&big, None));
    xml.push_str(&two_col_cont);
    let body = body_blocks(&xml);
    let pf = flow(&body, &two_col_cont, false);
    assert_eq!(
        pf.ranges(),
        vec![vec![(0, 32), (32, 33)], vec![(33, 35)]],
        "{:?}",
        pf.ranges()
    );
    assert_eq!(pf.pages[1].bands[0].cols, vec![(33, 35)]);
}

#[test]
fn oversize_block_after_closed_bands_gets_its_own_page() {
    // A block taller than a whole sheet after closed bands: no infinite
    // loop; it lands alone on a fresh sheet (the oversize exception holds
    // only for the page's first band).
    let huge = "H".repeat(20_000);
    let cont = letter(r#"<w:type w:val="continuous"/>"#);
    let body = body_blocks(&format!(
        "{}{}{}{}",
        para("One", Some(&letter(""))),
        para("Two", None),
        para(&huge, None),
        cont
    ));
    let pf = flow(&body, &cont, false);
    assert_eq!(pf.pages.len(), 2, "{:?}", pf.ranges());
    // The huge paragraph owns the fresh sheet; the zero-height trailing
    // sectPr block rides in its column (no metadata-only page).
    assert_eq!(pf.ranges()[1], vec![(2, 4)]);
}

#[test]
fn single_explicit_column_keeps_its_width() {
    // One explicit w:col: the band's drawn width is the configured column
    // width (SectionBox::single_col_w), not the full text width.
    let sect = letter(r#"<w:cols w:num="1" w:equalWidth="0"><w:col w:w="4320"/></w:cols>"#);
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    assert_eq!(pf.sections[0].col_w, vec![4320]);
    assert_eq!(pf.sections[0].single_col_w(), Some(4320));
}

#[test]
fn huge_column_space_does_not_overflow() {
    // An imported w:space near i32::MAX must not overflow the equal-width
    // math; widths clamp to >= 1.
    let sect = letter(r#"<w:cols w:num="3" w:space="2147483647"/>"#);
    let body = body_blocks(&para("One", Some(&sect)));
    let pf = flow(&body, &sect, false);
    assert_eq!(pf.sections[0].col_w, vec![1, 1, 1]);
    assert_eq!(pf.pages.len(), 1, "{:?}", pf.ranges());
}

#[test]
fn later_band_skips_no_columns() {
    // 33 one-line paragraphs leave ~62px of the sheet; the continuous
    // section's unequal columns (2640/6000, Word's Left preset) see an
    // 80-char paragraph estimate ~85px in narrow column 0 (does not fit)
    // but ~45px in wide column 1 (fits). It must not be silently placed in
    // column 1 and drawn in column 0: the band moves to a new sheet and the
    // paragraph lands there in column 0, so a band's ranges stay positional.
    let unequal_cont = letter(
        r#"<w:cols w:num="2" w:equalWidth="0"><w:col w:w="2640" w:space="720"/><w:col w:w="6000"/></w:cols><w:type w:val="continuous"/>"#,
    );
    let mut xml = String::new();
    for i in 0..33 {
        let sect = (i == 32).then(|| letter(""));
        xml.push_str(&para(&format!("p{i}"), sect.as_deref()));
    }
    xml.push_str(&para(&"P".repeat(80), None));
    xml.push_str(&unequal_cont);
    let body = body_blocks(&xml);
    let pf = flow(&body, &unequal_cont, false);
    assert_eq!(
        pf.ranges(),
        vec![vec![(0, 33)], vec![(33, 35)]],
        "{:?}",
        pf.ranges()
    );
    assert_eq!(pf.pages[0].bands.len(), 1);
}

#[test]
fn band_ranges_fit_their_columns() {
    // Every occupied range's estimated height at its own column width fits
    // the sheet's content height, except the fresh-sheet oversize exception
    // (a block taller than the page still gets its own sheet).
    let mut xml = String::new();
    for i in 0..40 {
        let sect = match i {
            19 => Some(letter(r#"<w:cols w:num="2"/>"#)),
            39 => Some(letter("")),
            _ => None,
        };
        xml.push_str(&para(&format!("p{i}"), sect.as_deref()));
    }
    let body = body_blocks(&xml);
    let cont = letter(r#"<w:type w:val="continuous"/>"#);
    let mut body = body;
    body.extend([
        Block::Paragraph(docxcore::model::Paragraph {
            content: vec![
                Inline::Run(docxcore::model::Run {
                    text: "after".into(),
                    ..Default::default()
                }),
                Inline::Break(docxcore::model::BreakKind::Page, Default::default()),
            ],
            ..Default::default()
        }),
        Block::Paragraph(docxcore::model::Paragraph {
            content: vec![Inline::Run(docxcore::model::Run {
                text: "tail".into(),
                ..Default::default()
            })],
            ..Default::default()
        }),
    ]);
    let pf = flow(&body, &cont, false);
    assert!(pf.pages.len() >= 2, "{:?}", pf.ranges());
    for page in &pf.pages {
        let owner = &pf.sections[page.bands[0].section];
        let content_h = (owner.h - owner.top - owner.bottom).max(1) as f32 / 15.0;
        for (bi, band) in page.bands.iter().enumerate() {
            let sb = &pf.sections[band.section];
            for (ci, &(s, e)) in band.cols.iter().enumerate() {
                let wpx = sb.col_w[ci].max(0) as f32 / 15.0;
                let h: f32 = (s..e).map(|i| crate::block_height_est(&body[i], wpx)).sum();
                assert!(
                    h <= content_h + 0.01 || (page.bands.len() == 1 && bi == 0),
                    "band {bi} range {ci} ({s},{e}) is {h}px of {content_h}px"
                );
            }
        }
    }
}

#[test]
fn zero_height_blocks_keep_the_column_empty() {
    // A zero-height block (an sdt wrapper, a sectPr) at a column's start
    // does not make the column "occupied": the page-tall table after it
    // moves to a fresh sheet, instead of the wrapper closing a metadata-only
    // band that forces the table over. The wrapper stays in its (heightless)
    // band on the first sheet; every block stays in exactly one range.
    let rows: Vec<docxcore::model::Row> = (0..30)
        .map(|_| docxcore::model::Row {
            cells: vec![],
            raw_props: vec![],
            property_change: None,
            element_attrs: Default::default(),
        })
        .collect();
    let cont = letter(r#"<w:type w:val="continuous"/>"#);
    let body = vec![
        body_blocks(&para("One", Some(&letter("")))).remove(0),
        Block::Raw(String::new()),
        Block::Table(docxcore::model::Table {
            rows,
            ..Default::default()
        }),
        body_blocks(&cont).remove(0),
    ];
    let pf = flow(&body, &cont, false);
    assert_eq!(
        pf.ranges(),
        vec![vec![(0, 1), (1, 2)], vec![(2, 4)]],
        "{:?}",
        pf.ranges()
    );
    let mut covered: Vec<usize> = Vec::new();
    for page in &pf.ranges() {
        for &(s, e) in page {
            covered.extend(s..e);
        }
    }
    assert_eq!(covered, (0..4).collect::<Vec<_>>(), "no block is lost");
}
