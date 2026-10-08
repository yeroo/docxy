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
pub(crate) fn is_sdt_open(raw: &str) -> bool {
    raw.trim_start().starts_with("<w:sdt>") && !raw.trim_end().ends_with("</w:sdt>")
}

pub(crate) fn is_sdt_close(raw: &str) -> bool {
    raw.trim_start().starts_with("</w:sdtContent>")
}

/// The closing boundary of a preserved smart tag (see `load::parse_smart_tag`).
pub(crate) const SMART_TAG_CLOSE: &str = "</w:smartTag>";

/// The opening boundary of a preserved smart tag: its start tag, with its
/// `w:smartTagPr` if it has one.
pub(crate) fn is_smart_tag_open(raw: &str) -> bool {
    raw.trim_start()
        .strip_prefix("<w:smartTag")
        .is_some_and(|rest| rest.starts_with(|c: char| c == '>' || c.is_whitespace()))
        && !raw.trim_end().ends_with(SMART_TAG_CLOSE)
}

pub(crate) fn is_smart_tag_close(raw: &str) -> bool {
    raw.trim() == SMART_TAG_CLOSE
}

/// The `w:docPartGallery` value in an SDT's properties, if any.
pub(crate) fn doc_part_gallery(raw: &str) -> Option<String> {
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
pub(crate) fn matching_close(blocks: &[Block], open: usize) -> Option<usize> {
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

/// The partner of each block content-control boundary, by index: opening
/// and closing boundaries paired the way the loader nested them.
fn sdt_pairs(blocks: &[Block]) -> Vec<(usize, usize)> {
    let mut open: Vec<usize> = Vec::new();
    let mut pairs = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let Block::Raw(raw) = b else { continue };
        if is_sdt_open(raw) {
            open.push(i);
        } else if is_sdt_close(raw) {
            if let Some(o) = open.pop() {
                pairs.push((o, i));
            }
        }
    }
    pairs
}

/// For a deletion of `blocks[from..=to]`: the index of each boundary outside
/// that range whose partner is inside it, ascending. Dropping those too
/// removes exactly the controls the deletion cut, and leaves every other
/// control (a neighbour, the one around them) with its own properties.
pub(crate) fn sdt_partners_outside(blocks: &[Block], from: usize, to: usize) -> Vec<usize> {
    let inside = |i: usize| (from..=to).contains(&i);
    let mut out: Vec<usize> = sdt_pairs(blocks)
        .into_iter()
        .filter_map(|(o, c)| match (inside(o), inside(c)) {
            (true, false) => Some(c),
            (false, true) => Some(o),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out
}

/// Drop every block content-control boundary in `blocks` that has no
/// partner (an opening one never closed, a closing one never opened), so the
/// blocks serialize to well-formed XML; the content between stays, as plain
/// blocks. Nested controls pair up innermost first. The indices dropped, in
/// ascending order (as they were before the removal).
pub fn balance_sdt_boundaries(blocks: &mut Vec<Block>) -> Vec<usize> {
    let mut open: Vec<usize> = Vec::new();
    let mut orphans: Vec<usize> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let Block::Raw(raw) = b else { continue };
        if is_sdt_open(raw) {
            open.push(i);
        } else if is_sdt_close(raw) && open.pop().is_none() {
            orphans.push(i);
        }
    }
    orphans.extend(open);
    orphans.sort_unstable();
    for &i in orphans.iter().rev() {
        blocks.remove(i);
    }
    orphans
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
            assert!(b.iter().any(is_page_number_open), "{sdt}");
            assert!(remove_page_numbers(&mut b));
            assert!(!b.iter().any(is_page_number_open));
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
        assert!(!b.iter().any(is_page_number_open));
        assert!(!remove_page_numbers(&mut b));
        assert_eq!(b, before);
    }

    /// Backspace at the start of a placed number's paragraph, Delete at its
    /// end, Select All + Delete and a selection crossing one boundary never
    /// leave one boundary of the control without the other, so the saved part
    /// stays well formed (`Editor::delete_selection` drops the control when
    /// it takes one of its boundaries).
    #[test]
    fn deleting_at_the_boundaries_keeps_the_control_balanced() {
        use crate::editor::{Caret, Editor};
        let balanced = |b: &[Block]| {
            let raws: Vec<&String> = b
                .iter()
                .filter_map(|b| match b {
                    Block::Raw(r) => Some(r),
                    _ => None,
                })
                .collect();
            raws.iter().filter(|r| is_sdt_open(r)).count()
                == raws.iter().filter(|r| is_sdt_close(r)).count()
        };
        let before = "<w:p><w:r><w:t>Left</w:t></w:r></w:p>";
        let after = "<w:p><w:r><w:t>Right</w:t></w:r></w:p>";
        let xml = format!(
            "{before}{}{after}",
            page_number_sdt_xml(&PAGE_NUMBER_DESIGNS[0], false, 1)
        );
        let para = 2; // Left, open boundary, the number, close boundary, Right
        let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
        ed.caret = Caret {
            path: vec![para],
            offset: 0,
        };
        ed.backspace();
        ed.backspace();
        assert!(balanced(&ed.doc.body), "{:?}", ed.doc.body);
        let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
        ed.caret = Caret {
            path: vec![para],
            offset: ed.doc.body[para].plain_text().chars().count(),
        };
        ed.delete_forward();
        ed.delete_forward();
        assert!(balanced(&ed.doc.body), "{:?}", ed.doc.body);
        let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
        ed.select_all();
        ed.delete_forward();
        assert!(balanced(&ed.doc.body), "{:?}", ed.doc.body);
        // A selection from inside "Left" into the number, and from the number
        // into "Right", each crossing one boundary: the control goes, the
        // text left over stays.
        for (from, to, left) in [
            ((0, 2), (para, 0), "Le1Right"),
            ((para, 0), (4, 2), "Leftght"),
        ] {
            let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
            ed.anchor = Some(Caret {
                path: vec![from.0],
                offset: from.1,
            });
            ed.caret = Caret {
                path: vec![to.0],
                offset: to.1,
            };
            ed.delete_forward();
            assert!(
                balanced(&ed.doc.body),
                "{from:?}..{to:?}: {:?}",
                ed.doc.body
            );
            assert!(!ed.doc.body.iter().any(|b| matches!(b, Block::Raw(_))));
            let text: String = ed.doc.body.iter().map(Block::plain_text).collect();
            assert_eq!(text, left);
            // The caret is still in a paragraph, where the selection began.
            let at = ed.caret.path[0];
            assert!(matches!(ed.doc.body[at], Block::Paragraph(_)));
        }
        // A cut across two neighbouring controls, and out of a nested one,
        // drops the controls it cut and keeps the others' own properties.
        let control = |name: &str, body: &str| {
            format!(
                "<w:sdt><w:sdtPr><w:alias w:val=\"{name}\"/></w:sdtPr><w:sdtContent>{body}</w:sdtContent></w:sdt>"
            )
        };
        let p = |t: &str| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>");
        let raws = |b: &[Block]| -> Vec<String> {
            b.iter()
                .filter_map(|b| match b {
                    Block::Raw(r) => Some(r.clone()),
                    _ => None,
                })
                .collect()
        };
        // Neighbours: [openA, pA, closeA, openB, pB, closeB], from pA to pB.
        let xml = format!("{}{}", control("A", &p("aa")), control("B", &p("bb")));
        let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
        ed.anchor = Some(Caret {
            path: vec![1],
            offset: 1,
        });
        ed.caret = Caret {
            path: vec![4],
            offset: 1,
        };
        ed.delete_forward();
        assert!(raws(&ed.doc.body).is_empty(), "{:?}", ed.doc.body);
        assert_eq!(ed.doc.body[ed.caret.path[0]].plain_text(), "ab");
        // Nested: [openO, openI, pI, closeI, pO, closeO], from pI into pO:
        // the inner control goes, the outer keeps its own properties.
        let xml = control("O", &format!("{}{}", control("I", &p("ii")), p("oo")));
        let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
        ed.anchor = Some(Caret {
            path: vec![2],
            offset: 1,
        });
        ed.caret = Caret {
            path: vec![4],
            offset: 1,
        };
        ed.delete_forward();
        let left = raws(&ed.doc.body);
        assert_eq!(left.len(), 2, "{:?}", ed.doc.body);
        assert!(left[0].contains("w:val=\"O\""), "{left:?}");
        assert!(balanced(&ed.doc.body));
        assert_eq!(ed.doc.body[ed.caret.path[0]].plain_text(), "io");
        // Select All + Delete over a number last or first in its part.
        let number = page_number_sdt_xml(&PAGE_NUMBER_DESIGNS[1], false, 2);
        for xml in [format!("{before}{number}"), format!("{number}{after}")] {
            let mut ed = Editor::new(crate::model::Document { body: blocks(&xml) });
            ed.select_all();
            ed.delete_forward();
            assert!(balanced(&ed.doc.body), "{xml}: {:?}", ed.doc.body);
            let out = crate::serialize::blocks_to_xml(&ed.doc.body);
            assert_eq!(
                out.matches("<w:sdt>").count(),
                out.matches("</w:sdt>").count()
            );
        }
    }

    #[test]
    fn unmatched_boundaries_are_dropped_and_pairs_kept() {
        let open = || Block::Raw("<w:sdt><w:sdtPr/><w:sdtContent>".into());
        let close = || Block::Raw("</w:sdtContent></w:sdt>".into());
        let p = || Block::Paragraph(Paragraph::default());
        let mut b = vec![
            open(),
            p(),
            close(),
            close(),
            p(),
            open(),
            open(),
            p(),
            close(),
        ];
        assert_eq!(balance_sdt_boundaries(&mut b), vec![3, 5]);
        assert_eq!(b, vec![open(), p(), close(), p(), open(), p(), close()]);
        assert!(balance_sdt_boundaries(&mut b).is_empty());
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
