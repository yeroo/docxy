//! Header and footer content helpers: the page numbers Insert › Page Number
//! places (#650).
//!
//! Word marks a placed page number by wrapping its paragraph in a block content
//! control whose `w:docPartGallery` is `Page Numbers (Top of Page)` or
//! `Page Numbers (Bottom of Page)`. The loader keeps a block `w:sdt` as two
//! `Block::Raw` boundaries around normally parsed, editable paragraphs (see
//! `load::parse_sdt_block`), so the same shape serves ours and Word's: Remove
//! Page Numbers finds either, and never touches a `PAGE` field typed by hand.

use crate::model::{Block, Paragraph};

/// Where a page-number design sits on its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberAlign {
    Left,
    Center,
    Right,
}

/// One design of the Top of Page / Bottom of Page galleries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageNumberDesign {
    pub name: &'static str,
    pub align: NumberAlign,
    /// "Page X of Y": `Page {PAGE} of {NUMPAGES}` instead of the bare number.
    pub of_total: bool,
}

/// The designs both galleries offer, in menu order.
pub const PAGE_NUMBER_DESIGNS: [PageNumberDesign; 4] = [
    PageNumberDesign {
        name: "Plain Number 1",
        align: NumberAlign::Left,
        of_total: false,
    },
    PageNumberDesign {
        name: "Plain Number 2",
        align: NumberAlign::Center,
        of_total: false,
    },
    PageNumberDesign {
        name: "Plain Number 3",
        align: NumberAlign::Right,
        of_total: false,
    },
    PageNumberDesign {
        name: "Page X of Y",
        align: NumberAlign::Center,
        of_total: true,
    },
];

/// The gallery value Word writes for a number at the top or bottom of the page.
pub fn page_number_gallery(top: bool) -> &'static str {
    if top {
        "Page Numbers (Top of Page)"
    } else {
        "Page Numbers (Bottom of Page)"
    }
}

fn field(instr: &str, cached: &str) -> String {
    format!(
        "<w:fldSimple w:instr=\" {instr} \"><w:r><w:t xml:space=\"preserve\">{cached}</w:t></w:r></w:fldSimple>"
    )
}

/// The block XML of a placed page number: Word's `docPartObj` content control
/// around one paragraph in the `Header` (`top`) or `Footer` style, holding the
/// design's `PAGE` field. `sdt_id` is the control's `w:id`.
pub fn page_number_sdt_xml(design: &PageNumberDesign, top: bool, sdt_id: u32) -> String {
    let style = if top { "Header" } else { "Footer" };
    let jc = match design.align {
        NumberAlign::Left => String::new(),
        NumberAlign::Center => "<w:jc w:val=\"center\"/>".into(),
        NumberAlign::Right => "<w:jc w:val=\"right\"/>".into(),
    };
    let body = if design.of_total {
        format!(
            "<w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>{}\
             <w:r><w:t xml:space=\"preserve\"> of </w:t></w:r>{}",
            field("PAGE", "1"),
            field("NUMPAGES", "1")
        )
    } else {
        field("PAGE", "1")
    };
    format!(
        "<w:sdt><w:sdtPr><w:id w:val=\"{sdt_id}\"/><w:docPartObj>\
         <w:docPartGallery w:val=\"{}\"/><w:docPartUnique/></w:docPartObj></w:sdtPr>\
         <w:sdtEndPr/><w:sdtContent><w:p><w:pPr><w:pStyle w:val=\"{style}\"/>{jc}</w:pPr>\
         {body}</w:p></w:sdtContent></w:sdt>",
        page_number_gallery(top)
    )
}

/// The opening boundary of a preserved block content control (not a
/// self-contained one without content).
fn is_sdt_open(raw: &str) -> bool {
    raw.trim_start().starts_with("<w:sdt>") && !raw.trim_end().ends_with("</w:sdt>")
}

fn is_sdt_close(raw: &str) -> bool {
    raw.trim_start().starts_with("</w:sdtContent>")
}

/// The `w:docPartGallery` value in an SDT's properties, if any.
fn doc_part_gallery(raw: &str) -> Option<String> {
    let (a, b) = crate::sect::find_element(raw, "w:docPartGallery")?;
    crate::load::xml_attr_value(&raw[a..b], "w:val")
}

/// Whether a block is the opening boundary of a placed page number (ours or
/// Word's: any `Page Numbers (…)` gallery).
pub fn is_page_number_open(block: &Block) -> bool {
    matches!(block, Block::Raw(raw) if is_sdt_open(raw)
        && doc_part_gallery(raw).is_some_and(|g| g.starts_with("Page Numbers")))
}

/// The index of the boundary closing the content control opened at `open`.
fn matching_close(blocks: &[Block], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, block) in blocks.iter().enumerate().skip(open) {
        let Block::Raw(raw) = block else { continue };
        if is_sdt_open(raw) {
            depth += 1;
        } else if is_sdt_close(raw) {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Whether `blocks` hold a placed page number.
pub fn has_page_number(blocks: &[Block]) -> bool {
    blocks.iter().any(is_page_number_open)
}

/// Remove every placed page number from a header or footer's blocks: each
/// `Page Numbers (…)` content control with all it holds. An unbalanced one
/// (no closing boundary) is left alone. A part left with no paragraph or
/// table gets one empty paragraph, which `w:hdr`/`w:ftr` require. Whether
/// anything was removed.
pub fn remove_page_numbers(blocks: &mut Vec<Block>) -> bool {
    let mut removed = false;
    let mut i = 0;
    while i < blocks.len() {
        if is_page_number_open(&blocks[i]) {
            if let Some(end) = matching_close(blocks, i) {
                blocks.drain(i..=end);
                removed = true;
                continue;
            }
        }
        i += 1;
    }
    if removed
        && !blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(_) | Block::Table(_)))
    {
        blocks.push(Block::Paragraph(Paragraph::default()));
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_header_footer};

    fn blocks(inner: &str) -> Vec<Block> {
        let xml = format!(
            "<w:ftr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{inner}</w:ftr>"
        );
        parse_header_footer(&xml, &Relationships::default())
    }

    /// What Word writes for Insert › Page Number › Bottom of Page › Plain Number 2.
    const WORD_SDT: &str = "<w:sdt><w:sdtPr><w:id w:val=\"-1318336367\"/><w:docPartObj>\
        <w:docPartGallery w:val=\"Page Numbers (Bottom of Page)\"/><w:docPartUnique/>\
        </w:docPartObj></w:sdtPr><w:sdtEndPr><w:rPr><w:noProof/></w:rPr></w:sdtEndPr>\
        <w:sdtContent><w:p><w:pPr><w:pStyle w:val=\"Footer\"/><w:jc w:val=\"center\"/></w:pPr>\
        <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE   \\* MERGEFORMAT </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:rPr><w:noProof/></w:rPr><w:t>2</w:t></w:r>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p></w:sdtContent></w:sdt>";

    const HAND_TYPED: &str = "<w:p><w:r><w:t xml:space=\"preserve\">Page </w:t></w:r>\
        <w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple></w:p>";

    #[test]
    fn word_and_our_page_numbers_are_detected_and_removed() {
        let ours = page_number_sdt_xml(&PAGE_NUMBER_DESIGNS[1], false, 7);
        for sdt in [WORD_SDT.to_string(), ours] {
            let mut b = blocks(&format!("{HAND_TYPED}{sdt}"));
            assert!(has_page_number(&b), "{sdt}");
            assert!(remove_page_numbers(&mut b));
            assert!(!has_page_number(&b));
            // The hand-typed PAGE field stays, and nothing of the control does.
            let xml = crate::serialize::blocks_to_xml(&b);
            assert!(xml.contains("w:instr=\" PAGE \""), "{xml}");
            assert!(!xml.contains("w:sdt"), "{xml}");
            assert_eq!(b.len(), 1);
        }
    }

    #[test]
    fn removing_the_only_content_leaves_one_empty_paragraph() {
        let mut b = blocks(&page_number_sdt_xml(&PAGE_NUMBER_DESIGNS[0], true, 1));
        assert!(remove_page_numbers(&mut b));
        assert_eq!(b, vec![Block::Paragraph(Paragraph::default())]);
    }

    #[test]
    fn other_content_controls_and_hand_typed_numbers_stay() {
        let other = "<w:sdt><w:sdtPr><w:docPartObj><w:docPartGallery w:val=\"Cover Pages\"/>\
            </w:docPartObj></w:sdtPr><w:sdtContent><w:p><w:r><w:t>c</w:t></w:r></w:p>\
            </w:sdtContent></w:sdt>";
        let mut b = blocks(&format!("{other}{HAND_TYPED}"));
        let before = b.clone();
        assert!(!has_page_number(&b));
        assert!(!remove_page_numbers(&mut b));
        assert_eq!(b, before);
    }

    #[test]
    fn our_page_number_round_trips_through_the_loader() {
        let design = PAGE_NUMBER_DESIGNS[3];
        let b = blocks(&page_number_sdt_xml(&design, false, 3));
        // Boundary, the editable paragraph, boundary.
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].plain_text(), "Page 1 of 1");
        let xml = crate::serialize::blocks_to_xml(&b);
        assert!(xml.contains("Page Numbers (Bottom of Page)"));
        assert!(xml.contains("<w:docPartUnique/>"));
        assert!(xml.contains("NUMPAGES"));
    }
}
