//! Live page values (#1437), held to the PDF export (`export::page_labels`),
//! the oracle for the format, restart and chapter rules.

use super::*;
use crate::hf::tests::{body_blocks, para};
use crate::page_flow::flow;
use docxcore::export::{PdfOptions, page_labels};
use docxcore::model::Document;
use docxcore::numbering::{heading_chapters, parse_numbering_xml};
use docxcore::styles::parse_styles_xml;
use std::rc::Rc;

fn sect(extra: &str) -> String {
    format!(
        "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/>\
         <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" \
         w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>{extra}</w:sectPr>"
    )
}

fn heading(text: &str, style: &str, sect_pr: Option<&str>) -> String {
    let ppr = sect_pr.unwrap_or_default();
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/>{ppr}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
    )
}

const NUMBERING: &str = r#"<w:numbering>
    <w:abstractNum w:abstractNumId="5">
        <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:pStyle w:val="Heading1"/><w:lvlText w:val="%1."/></w:lvl>
        <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:pStyle w:val="Heading2"/><w:lvlText w:val="%1.%2"/></w:lvl>
    </w:abstractNum>
    <w:num w:numId="7"><w:abstractNumId w:val="5"/></w:num>
    </w:numbering>"#;

const STYLES: &str = r#"<w:styles>
    <w:style w:type="paragraph" w:styleId="Heading1"><w:pPr><w:numPr><w:numId w:val="7"/></w:numPr></w:pPr></w:style>
    <w:style w:type="paragraph" w:styleId="Heading2"><w:basedOn w:val="Heading1"/><w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:style>
    <w:style w:type="paragraph" w:styleId="Plain"/>
    </w:styles>"#;

/// The page view's values and the export's labels for `xml` (its last
/// section is `last`): what both say, page by page.
fn both(xml: &str, last: &str) -> (Vec<PageVals>, Vec<String>) {
    let body = body_blocks(&format!("{xml}{last}"));
    let pf = flow(&body, last, false);
    let doc = Document { body };
    let num = parse_numbering_xml(NUMBERING);
    let styles = parse_styles_xml(STYLES);
    let heads = heading_chapters(&doc, &num, &styles);
    let opts = PdfOptions {
        numbering: Rc::new(num),
        styles: Rc::new(styles),
        ..Default::default()
    };
    (page_values(&pf, &heads), page_labels(&doc, &opts))
}

fn pages(v: &[PageVals]) -> Vec<&str> {
    v.iter().map(|p| p.page.as_str()).collect()
}

fn agree(xml: &str, last: &str) -> Vec<String> {
    let (vals, labels) = both(xml, last);
    let got: Vec<String> = vals.iter().map(|p| p.page.clone()).collect();
    assert_eq!(got, labels, "the page view and the export disagree");
    got
}

#[test]
fn roman_and_letter_formats_apply_per_section() {
    let roman = sect(r#"<w:pgNumType w:fmt="lowerRoman"/>"#);
    let letter = sect(r#"<w:pgNumType w:fmt="upperLetter"/>"#);
    let xml = format!(
        "{}{}{}",
        para("a", Some(&roman)),
        para("b", Some(&roman)),
        para("c", Some(&letter))
    );
    let got = agree(&xml, &sect(""));
    // The trailing (decimal) section is a page of its own.
    assert_eq!(got, vec!["i", "ii", "C", "4"]);
}

#[test]
fn start_restarts_a_section_and_continuations_keep_counting() {
    let five = sect(r#"<w:pgNumType w:start="5"/>"#);
    let plain = sect("");
    let two = sect(r#"<w:pgNumType w:start="2"/>"#);
    let xml = format!(
        "{}{}{}{}",
        para("a", Some(&five)),
        para("b", Some(&plain)),
        para("c", Some(&two)),
        para("d", Some(&plain)),
    );
    let got = agree(&xml, &sect(""));
    assert_eq!(got, vec!["5", "6", "2", "3", "4"]);
}

#[test]
fn a_negative_or_huge_start_is_clamped_as_the_export_does() {
    let neg = sect(r#"<w:pgNumType w:start="-3"/>"#);
    let huge = sect(r#"<w:pgNumType w:start="99999999"/>"#);
    let xml = format!("{}{}", para("a", Some(&neg)), para("b", Some(&huge)));
    let got = agree(&xml, &sect(""));
    assert_eq!(got, vec!["0", "32767", "32768"]);
}

#[test]
fn a_filler_page_takes_a_number() {
    // Page 1 is odd; the oddPage section with start=4 needs a filler first.
    let odd4 = sect(r#"<w:type w:val="oddPage"/><w:pgNumType w:start="4"/>"#);
    let xml = para("a", Some(&sect("")));
    let (vals, labels) = both(&xml, &odd4);
    assert_eq!(pages(&vals), vec!["1", "2", "4"]);
    assert_eq!(
        vals.iter().map(|p| p.page.clone()).collect::<Vec<_>>(),
        labels
    );
    // The filler counts toward NUMPAGES and its (previous) section's pages.
    assert_eq!(vals[0].num_pages, "3");
    assert_eq!(vals[0].section_pages, "2");
    assert_eq!(vals[2].section_pages, "1");
}

#[test]
fn chapter_prefix_names_the_first_heading_on_the_page_else_the_last_before() {
    let ch = sect(r#"<w:pgNumType w:chapStyle="1" w:chapSep="hyphen"/>"#);
    let two_on_a_page = format!(
        "{}{}",
        heading("Two", "Heading1", None),
        heading("Three", "Heading1", Some(&ch))
    );
    let xml = format!(
        "{}{}{}{}{}",
        heading("One", "Heading1", Some(&ch)),
        para("body", Some(&ch)),
        // Two headings on one page: the first counts.
        two_on_a_page,
        para("tail", Some(&ch)),
        // No heading of the level, no numbering: plain.
        para("end", Some(&sect(""))),
    );
    let got = agree(&xml, &ch);
    assert_eq!(got, vec!["1-1", "1-2", "2-3", "3-4", "5", "3-6"]);
}

#[test]
fn an_unnumbered_heading_and_a_missing_one_fall_back_to_the_plain_number() {
    let ch = sect(r#"<w:pgNumType w:chapStyle="1" w:chapSep="period" w:fmt="lowerRoman"/>"#);
    // numId 0 switches the heading's numbering off: an unnumbered chapter.
    let unnumbered = heading("Unnumbered", "Heading1", Some(&ch)).replace(
        "<w:pStyle w:val=\"Heading1\"/>",
        "<w:pStyle w:val=\"Heading1\"/><w:numPr><w:numId w:val=\"0\"/></w:numPr>",
    );
    let xml = format!(
        "{}{}{}{}",
        para("no heading yet", Some(&ch)),
        heading("One", "Heading1", Some(&ch)),
        para("carries one", Some(&ch)),
        unnumbered,
    );
    let got = agree(&xml, &ch);
    // No heading, chapter 1 (and carried), then the unnumbered one: plain.
    assert_eq!(&got[..4], ["i", "1.ii", "1.iii", "iv"]);
}

#[test]
fn chapter_level_two_uses_subheading_numbers() {
    let ch = sect(r#"<w:pgNumType w:chapStyle="2" w:chapSep="colon"/>"#);
    let xml = format!(
        "{}{}",
        heading("A", "Heading1", None) + &heading("A1", "Heading2", Some(&ch)),
        heading("B", "Heading1", None) + &heading("B1", "Heading2", Some(&ch)),
    );
    let got = agree(&xml, &ch);
    assert_eq!(got, vec!["1.1:1", "2.1:2", "2.1:3"]);
}

#[test]
fn num_pages_and_section_pages_count_pages() {
    let a = sect("");
    let xml = format!(
        "{}{}{}",
        para("a", Some(&a)),
        para("b", Some(&a)),
        para("c", Some(&a))
    );
    let (vals, _) = both(&xml, &sect(""));
    assert_eq!(vals.len(), 4);
    for v in &vals {
        assert_eq!(v.num_pages, "4");
        assert_eq!(v.section_pages, "1");
    }
}

#[test]
fn a_continuous_section_shares_its_pages_sections_count() {
    let cont = sect(r#"<w:type w:val="continuous"/>"#);
    let xml = format!("{}{}", para("a", Some(&sect(""))), para("b", Some(&cont)));
    let (vals, _) = both(&xml, &cont);
    assert_eq!(vals.len(), 1);
    // The owning section is the first; the page counts for both sections.
    assert_eq!(vals[0].section_pages, "1");
}

#[test]
fn value_picks_the_field() {
    let v = PageVals {
        page: "iii".into(),
        num_pages: "9".into(),
        section_pages: "4".into(),
    };
    assert_eq!(v.value(PageField::Page), "iii");
    assert_eq!(v.value(PageField::NumPages), "9");
    assert_eq!(v.value(PageField::SectionPages), "4");
}

/// #1437 r1: the chapters are numbered over the document the flow was made
/// from. A rejected insertion shifts the blocks of the Original view, so
/// numbering the live body would put a chapter on the wrong page.
#[test]
fn chapters_are_numbered_over_the_document_that_was_flowed() {
    use docxcore::markup::MarkupView;
    let ch = sect(r#"<w:pgNumType w:chapStyle="1"/>"#);
    let inserted = "<w:p><w:pPr><w:rPr><w:ins w:id=\"1\" w:author=\"A\"/></w:rPr></w:pPr>\
        <w:ins w:id=\"2\" w:author=\"A\"><w:r><w:t>new</w:t></w:r></w:ins></w:p>";
    let xml = format!(
        "{inserted}{}{}",
        heading("One", "Heading1", Some(&ch)),
        heading("Two", "Heading1", None),
    );
    let body = body_blocks(&format!("{xml}{ch}"));
    let live = Document { body };
    let shown = live.markup_view(MarkupView::Original);
    assert_ne!(shown.body.len(), live.body.len(), "the view drops a block");
    let pf = flow(&shown.body, &ch, false);
    let styles = parse_styles_xml(STYLES);
    let vals = |doc: &Document| {
        let heads = chapter_headings(doc, &pf, Some(NUMBERING.as_bytes()), &styles);
        pages(&page_values(&pf, &heads))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    // What the page view shows for chapters 1 and 2 (one section: one page
    // per its own flow, so compare against the shown document's labels).
    let right = vals(&shown);
    let wrong = vals(&live);
    assert_ne!(right, wrong, "numbering the live body lands on other pages");
    assert!(right[0].starts_with("1-"), "{right:?}");
}

#[test]
fn a_numbered_heading_in_a_table_never_sets_the_chapter() {
    let ch = sect(r#"<w:pgNumType w:chapStyle="1"/>"#);
    let table = format!(
        "<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>",
        heading("In a cell", "Heading1", None)
    );
    let xml = format!(
        "{}{}{}",
        heading("One", "Heading1", Some(&ch)),
        // Page 2: only a table whose cell is a numbered heading (chapter 2).
        table + &para("after", Some(&ch)),
        para("page three", Some(&ch)),
    );
    let got = agree(&xml, &ch);
    // The cell's heading moves the counter but is no chapter: page 2 keeps
    // chapter 1, as the export has it.
    assert_eq!(&got[..3], ["1-1", "1-2", "1-3"]);
}

#[test]
fn a_start_past_i32_clamps_like_the_export() {
    let wide = sect(r#"<w:pgNumType w:start="4294967295"/>"#);
    let xml = format!("{}{}", para("a", Some(&wide)), para("b", Some(&sect(""))));
    let got = agree(&xml, &sect(""));
    assert_eq!(&got[..2], ["32767", "32768"]);
}

#[test]
fn a_continuous_section_flowing_onto_page_two_counts_both_pages() {
    let cont = sect(r#"<w:type w:val="continuous"/>"#);
    let mut xml = para("first section", Some(&sect("")));
    for i in 0..120 {
        xml.push_str(&para(&format!("line {i}"), None));
    }
    let (vals, _) = both(&xml, &cont);
    assert!(vals.len() >= 2, "the continuous section flows on");
    // Page 1 holds both sections and is owned by the first (1 page); every
    // later page is the second section's, which is on page 1 too.
    let n = vals.len().to_string();
    assert_eq!(vals[0].section_pages, "1");
    assert_eq!(vals[1].section_pages, n);
    assert_eq!(vals[0].num_pages, n);
}
