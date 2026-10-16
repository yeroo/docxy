//! Manual hyphenation's editor side (#1266): the walk over words that may take
//! an optional hyphen, and inserting one as `w:softHyphen`.

use docxcore::editor::Editor;
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline};
use docxcore::serialize::document_to_xml;

fn load(body: &str) -> Document {
    parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &Relationships::default(),
    )
}

fn editor(paras: &str) -> Editor {
    Editor::new(load(paras))
}

fn text(ed: &Editor, para: usize) -> String {
    match &ed.doc.body[para] {
        Block::Paragraph(p) => p.content.iter().map(Inline::text).collect(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

const P0: &[usize] = &[0];

fn words(ed: &Editor, caps: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = (vec![0], 0);
    while let Some(c) = ed.next_hyphen_candidate((&at.0, at.1), None, caps) {
        out.push(c.word.clone());
        at = (c.path, c.end);
    }
    out
}

#[test]
fn the_walk_finds_only_long_plain_words() {
    let ed = editor(
        "<w:p><w:r><w:t>The computer has 12345678 and co-operate, hyphenation \
         fine ab\u{ad}cdefgh don't plus</w:t></w:r></w:p>",
    );
    assert_eq!(words(&ed, true), ["computer", "hyphenation"]);
}

#[test]
fn capitals_are_skipped_unless_allowed() {
    let ed = editor("<w:p><w:r><w:t>COMPUTER computer Computer</w:t></w:r></w:p>");
    assert_eq!(words(&ed, true), ["COMPUTER", "computer", "Computer"]);
    assert_eq!(words(&ed, false), ["computer", "Computer"]);
}

#[test]
fn the_walk_starts_at_the_caret_and_stops_at_the_end_of_a_selection() {
    let ed = editor("<w:p><w:r><w:t>computer hyphenation wanted</w:t></w:r></w:p>");
    let c = ed.next_hyphen_candidate((P0, 8), None, true).unwrap();
    assert_eq!((c.word.as_str(), c.start, c.end), ("hyphenation", 9, 20));
    // Mid-word starts skip that word.
    let c = ed.next_hyphen_candidate((P0, 1), None, true).unwrap();
    assert_eq!(c.word, "hyphenation");
    // A limit that cuts a word off excludes it.
    let c = ed
        .next_hyphen_candidate((P0, 0), Some((P0, 15)), true)
        .unwrap();
    assert_eq!(c.word, "computer");
    assert!(
        ed.next_hyphen_candidate((P0, 8), Some((P0, 15)), true)
            .is_none()
    );
}

#[test]
fn the_walk_crosses_paragraphs() {
    let ed =
        editor("<w:p><w:r><w:t>tiny</w:t></w:r></w:p><w:p><w:r><w:t>computer</w:t></w:r></w:p>");
    let c = ed.next_hyphen_candidate((P0, 0), None, true).unwrap();
    assert_eq!((c.path, c.word.as_str()), (vec![1], "computer"));
}

#[test]
fn a_word_inside_a_field_result_is_not_found() {
    let ed = editor(
        "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:instrText xml:space=\"preserve\"> REF x </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
         <w:r><w:t>hyphenation</w:t></w:r>\
         <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
         <w:r><w:t xml:space=\"preserve\"> computer</w:t></w:r></w:p>",
    );
    // Only the word after the field: the result inside it is one unit.
    assert_eq!(words(&ed, true), ["computer"]);
}

#[test]
fn a_soft_hyphen_is_one_step_written_as_the_element() {
    let mut ed = editor("<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>computer works</w:t></w:r></w:p>");
    let before = document_to_xml(&ed.doc);
    assert!(ed.insert_soft_hyphen(P0, 3));
    assert_eq!(text(&ed, 0), "com\u{ad}puter works");
    let xml = document_to_xml(&ed.doc);
    assert!(xml.contains("<w:softHyphen/>"), "{xml}");
    assert!(!xml.contains('\u{ad}'), "{xml}");
    // The save reads back to the same text and formatting stays on all of it.
    let back = parse_document_xml(&xml, &Relationships::default());
    assert_eq!(
        match &back.body[0] {
            Block::Paragraph(p) => p.content.iter().map(Inline::text).collect::<String>(),
            _ => unreachable!(),
        },
        "com\u{ad}puter works"
    );
    assert_eq!(
        xml.matches("<w:b/>").count(),
        xml.matches("<w:r>").count(),
        "{xml}"
    );
    assert!(ed.undo());
    assert_eq!(document_to_xml(&ed.doc), before);
}

#[test]
fn two_hyphens_in_one_run_each_take_a_step() {
    let mut ed = editor("<w:p><w:r><w:t>computer hyphenation</w:t></w:r></w:p>");
    assert!(ed.insert_soft_hyphen(P0, 3));
    // The word after it moved by the one inserted character.
    let c = ed.next_hyphen_candidate((P0, 4 + 6), None, true).unwrap();
    assert_eq!(c.word, "hyphenation");
    assert!(ed.insert_soft_hyphen(&c.path, c.start + 7));
    assert_eq!(text(&ed, 0), "com\u{ad}puter hyphena\u{ad}tion");
    assert!(ed.undo());
    assert_eq!(text(&ed, 0), "com\u{ad}puter hyphenation");
    assert!(ed.undo());
    assert_eq!(text(&ed, 0), "computer hyphenation");
    // Both words already hold a hyphen once done: the walk finds nothing.
    assert!(ed.redo() && ed.redo());
    assert!(words(&ed, true).is_empty());
}

#[test]
fn an_offset_past_the_paragraph_inserts_nothing() {
    let mut ed = editor("<w:p><w:r><w:t>computer</w:t></w:r></w:p>");
    assert!(!ed.insert_soft_hyphen(P0, 99));
    assert!(!ed.insert_soft_hyphen(&[7], 0));
    assert_eq!(text(&ed, 0), "computer");
}

#[test]
fn a_loose_field_result_across_paragraphs_is_not_found() {
    // A TOC-shaped field: begin in one paragraph, end in another, its result
    // (editable text) between them.
    let ed = editor(
        "<w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:instrText xml:space=\"preserve\"> TOC \\o </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
         <w:r><w:t>hyphenation</w:t></w:r></w:p>\
         <w:p><w:r><w:t>computer</w:t></w:r></w:p>\
         <w:p><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
         <w:r><w:t xml:space=\"preserve\"> wanted</w:t></w:r></w:p>\
         <w:p><w:r><w:t>beautiful</w:t></w:r></w:p>",
    );
    let mut at = (vec![0], 0);
    let mut found = Vec::new();
    while let Some(c) = ed.next_hyphen_candidate((&at.0, at.1), None, true) {
        found.push(c.word.clone());
        at = (c.path, c.end);
    }
    assert_eq!(found, ["wanted", "beautiful"]);
}

#[test]
fn a_word_in_a_hyperlink_is_not_found_and_the_link_is_untouched() {
    let mut ed = editor(
        "<w:p><w:hyperlink w:anchor=\"x\"><w:r><w:t>hyphenation</w:t></w:r></w:hyperlink>\
         <w:r><w:t xml:space=\"preserve\"> computer</w:t></w:r></w:p>",
    );
    assert_eq!(words(&ed, true), ["computer"]);
    let c = ed.next_hyphen_candidate((P0, 0), None, true).unwrap();
    assert!(ed.insert_soft_hyphen(&c.path, c.start + c.point));
    let xml = document_to_xml(&ed.doc);
    assert_eq!(xml.matches("<w:hyperlink").count(), 1, "{xml}");
}

#[test]
fn a_tracked_change_does_not_hide_the_words_around_it() {
    let ed = editor(
        "<w:p><w:r><w:t xml:space=\"preserve\">computer </w:t></w:r>\
         <w:ins w:id=\"1\" w:author=\"a\"><w:r><w:t>inserted</w:t></w:r></w:ins>\
         <w:del w:id=\"2\" w:author=\"a\"><w:r><w:delText>deleted</w:delText></w:r></w:del>\
         <w:r><w:t xml:space=\"preserve\"> wanted</w:t></w:r></w:p>",
    );
    assert_eq!(words(&ed, true), ["computer", "wanted"]);
}

#[test]
fn a_field_opened_inside_a_tracked_change_still_bars_what_follows() {
    // The begin sits in an insertion, the end after the next word: the word
    // between is a field result.
    let ed = editor(
        "<w:p><w:ins w:id=\"1\" w:author=\"a\"><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:instrText xml:space=\"preserve\"> TOC </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r></w:ins>\
         <w:r><w:t xml:space=\"preserve\">hyphenation </w:t></w:r>\
         <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
         <w:r><w:t xml:space=\"preserve\"> computer</w:t></w:r></w:p>",
    );
    assert_eq!(words(&ed, true), ["computer"]);
}
