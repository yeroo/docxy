use super::*;

/// The shape Word writes for a cover from its gallery: placeholder controls
/// with `w:showingPlcHdr` and a data binding, a date picker named `Publish
/// Date`, and an Abstract held by a block-level control over two paragraphs.
pub(crate) const WORD_COVER: &str = "<w:sdt><w:sdtPr><w:id w:val=\"-1876379021\"/><w:docPartObj>\
    <w:docPartGallery w:val=\"Cover Pages\"/><w:docPartUnique/></w:docPartObj></w:sdtPr>\
    <w:sdtEndPr><w:rPr><w:sz w:val=\"22\"/></w:rPr></w:sdtEndPr><w:sdtContent>\
    <w:p><w:r><w:t>Logo</w:t></w:r></w:p>\
    <w:p><w:sdt><w:sdtPr><w:rPr><w:sz w:val=\"72\"/></w:rPr><w:alias w:val=\"Title\"/><w:tag w:val=\"\"/>\
    <w:id w:val=\"1714375384\"/><w:showingPlcHdr/><w:dataBinding w:prefixMappings=\"xmlns:ns0='http://purl.org/dc/elements/1.1/' \" \
    w:xpath=\"/ns1:coreProperties[1]/ns0:title[1]\" w:storeItemID=\"{6C3C8BC8-F283-45AE-878A-BAB7291924A1}\"/><w:text/></w:sdtPr>\
    <w:sdtContent><w:r><w:rPr><w:sz w:val=\"72\"/></w:rPr><w:t>Annual Report</w:t></w:r></w:sdtContent></w:sdt></w:p>\
    <w:p><w:sdt><w:sdtPr><w:alias w:val=\"Subtitle\"/><w:id w:val=\"-2\"/><w:showingPlcHdr/></w:sdtPr>\
    <w:sdtContent><w:r><w:t>[Document subtitle]</w:t></w:r></w:sdtContent></w:sdt></w:p>\
    <w:sdt><w:sdtPr><w:alias w:val=\"Abstract\"/><w:id w:val=\"3\"/></w:sdtPr><w:sdtContent>\
    <w:p><w:r><w:t>First line.</w:t></w:r></w:p><w:p><w:r><w:t>Second line.</w:t></w:r></w:p>\
    </w:sdtContent></w:sdt>\
    <w:p><w:sdt><w:sdtPr><w:alias w:val=\"Publish Date\"/><w:id w:val=\"4\"/><w:date/></w:sdtPr>\
    <w:sdtContent><w:r><w:t>2026-10-02</w:t></w:r></w:sdtContent></w:sdt></w:p>\
    <w:p><w:r><w:br w:type=\"page\"/></w:r></w:p></w:sdtContent></w:sdt>";

fn aliases(blocks: &[Block]) -> Vec<Placeholder> {
    let mut out = Vec::new();
    for b in blocks {
        if let Block::Paragraph(p) = b {
            for i in &p.content {
                if let Inline::Raw(raw) = i {
                    if is_sdt_open(raw) {
                        out.extend(placeholder_of(raw));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn every_design_parses_back_to_one_cover_with_its_placeholders() {
    for design in &COVER_DESIGNS {
        let ids: Vec<i64> = (10..20).collect();
        let xml = cover_sdt_xml(design, &Typed::new(), &ids);
        let blocks = body_blocks(&xml);
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, Block::SectionProperties(_))),
            "{xml}"
        );
        assert_eq!(find_cover(&blocks), Some((0, blocks.len() - 1)), "{xml}");
        let want: Vec<Placeholder> = design.paras.iter().filter_map(|p| p.field).collect();
        assert_eq!(aliases(&blocks), want, "{}", design.name);
        // Untouched, nothing reads as typed.
        assert!(typed_placeholders(&blocks).is_empty(), "{}", design.name);
        // Every control has its own id.
        let mut seen = sdt_ids(&xml);
        assert_eq!(seen.len(), want.len() + 1);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), want.len() + 1);
        // The cover ends with the page break, inside the control.
        let Some(Block::Paragraph(last)) = blocks.get(blocks.len() - 2) else {
            panic!("{xml}");
        };
        assert!(matches!(
            last.content.as_slice(),
            [Inline::Break(crate::model::BreakKind::Page, _)]
        ));
        // Placeholders show their prompts, and the design's own formatting.
        let text: String = blocks.iter().map(Block::plain_text).collect();
        for field in &want {
            assert!(text.contains(field.prompt()), "{text}");
        }
        assert!(!xml.contains("w:pStyle") && !xml.contains("showingPlcHdr"));
    }
}

#[test]
fn typed_text_round_trips_through_a_design() {
    let typed: Typed = [
        (Placeholder::Title, "Q3 & Q4 <draft>".to_string()),
        (Placeholder::Abstract, "One\nTwo\tthree".to_string()),
    ]
    .into();
    let xml = cover_sdt_xml(&COVER_DESIGNS[2], &typed, &[1, 2, 3, 4, 5, 6, 7]);
    let blocks = body_blocks(&xml);
    assert_eq!(typed_placeholders(&blocks), typed, "{xml}");
}

#[test]
fn a_word_cover_reads_its_typed_text_and_skips_its_prompts() {
    let blocks = body_blocks(WORD_COVER);
    assert!(find_cover(&blocks).is_some());
    let typed = typed_placeholders(&blocks);
    // Typed into a control that still says showingPlcHdr: carried anyway.
    assert_eq!(
        typed.get(&Placeholder::Title).map(String::as_str),
        Some("Annual Report")
    );
    assert_eq!(typed.get(&Placeholder::Subtitle), None);
    assert_eq!(
        typed.get(&Placeholder::Abstract).map(String::as_str),
        Some("First line.\nSecond line.")
    );
    assert_eq!(
        typed.get(&Placeholder::Date).map(String::as_str),
        Some("2026-10-02")
    );
    assert_eq!(typed.len(), 3);
}

#[test]
fn find_cover_skips_other_controls_and_an_unclosed_one() {
    let page_number = crate::hf::page_number_sdt_xml(&crate::hf::PAGE_NUMBER_DESIGNS[0], true, 5);
    let cover = cover_sdt_xml(&COVER_DESIGNS[0], &Typed::new(), &[1, 2, 3, 4, 5]);
    let blocks = body_blocks(&format!("<w:p/>{page_number}{cover}<w:p/>"));
    let (open, close) = find_cover(&blocks).unwrap();
    assert!(is_cover_open(&blocks[open]));
    assert_eq!(close, blocks.len() - 2);
    assert_eq!(find_cover(&body_blocks(&page_number)), None);
    // A cover whose closing boundary is gone is not one Remove can take.
    let mut cut = blocks.clone();
    cut.truncate(close);
    assert_eq!(find_cover(&cut), None);
}

#[test]
fn aliases_match_word_names_and_tags() {
    assert_eq!(
        Placeholder::from_alias("Publish Date"),
        Some(Placeholder::Date)
    );
    assert_eq!(Placeholder::from_alias(" title "), Some(Placeholder::Title));
    assert_eq!(Placeholder::from_alias("Company Address"), None);
    // No alias: the tag names it.
    let open = "<w:sdt><w:sdtPr><w:tag w:val=\"Author\"/></w:sdtPr><w:sdtContent>";
    assert_eq!(placeholder_of(open), Some(Placeholder::Author));
}

#[test]
fn ids_avoid_the_used_ones() {
    assert_eq!(
        sdt_ids("<w:sdtPr><w:id w:val=\"-7\"/></w:sdtPr><w:id w:val=\"12\"/>"),
        vec![-7, 12]
    );
    assert_eq!(free_sdt_ids(&[-7, 12], 3), vec![13, 14, 15]);
    assert_eq!(free_sdt_ids(&[], 2), vec![1, 2]);
    // Ids Word cannot write are none of ours to avoid.
    assert_eq!(
        sdt_ids("<w:id w:val=\"9223372036854775807\"/><w:id w:val=\"2147483648\"/>"),
        Vec::<i64>::new()
    );
    assert_eq!(free_sdt_ids(&[i64::MAX], 2), vec![1, 2]);
    assert_eq!(free_sdt_ids(&[i64::from(i32::MAX)], 1), vec![1]);
    assert_eq!(
        free_sdt_ids(&[i64::from(i32::MAX) - 1], 1),
        vec![i64::from(i32::MAX)]
    );
    // Past Word's range: the smallest free positive ones.
    assert_eq!(
        free_sdt_ids(&[1, 3, i64::from(i32::MAX) - 1], 3),
        vec![2, 4, 5]
    );
}
