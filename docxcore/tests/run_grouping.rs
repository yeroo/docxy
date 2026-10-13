//! A `w:r` with several children saves as that one `w:r` again (#1216): the
//! model holds one inline per `w:t` / `w:tab` / `w:br`, and the save groups
//! the ones that shared a `w:r` back together. `w:lastRenderedPageBreak`
//! stays before the child it preceded, and an explicit
//! `w:type="textWrapping"` on a `w:br` is kept.

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, strictly_equal};
use docxcore::serialize::document_to_xml;

fn load(body: &str) -> Document {
    parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &Relationships::default(),
    )
}

/// The saved `w:p` elements of a document.
fn saved_paras(doc: &Document) -> Vec<String> {
    let xml = document_to_xml(doc);
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(start) = rest.find("<w:p>").or_else(|| rest.find("<w:p ")) {
        let end = rest[start..].find("</w:p>").unwrap() + start + "</w:p>".len();
        out.push(rest[start..end].to_string());
        rest = &rest[end..];
    }
    out
}

fn saved_para(doc: &Document) -> String {
    saved_paras(doc).remove(0)
}

/// `w:r` elements in `xml` (not `w:rPr`).
fn run_count(xml: &str) -> usize {
    xml.matches("<w:r>").count() + xml.matches("<w:r ").count()
}

fn markers(doc: &Document) -> usize {
    document_to_xml(doc)
        .matches("<w:lastRenderedPageBreak/>")
        .count()
}

const T: &str = "<w:t xml:space=\"preserve\">";

#[test]
fn text_tab_text_saves_as_one_run() {
    let doc = load(
        "<w:p><w:r w:rsidR=\"00A1\"><w:rPr><w:b/></w:rPr><w:t>a</w:t><w:tab/>\
         <w:t>b</w:t></w:r></w:p>",
    );
    let p = saved_para(&doc);
    assert_eq!(run_count(&p), 1, "{p}");
    assert_eq!(p.matches("<w:rPr>").count(), 1, "{p}");
    assert!(
        p.contains(&format!(
            "<w:r w:rsidR=\"00A1\"><w:rPr><w:b/></w:rPr>{T}a</w:t><w:tab/>{T}b</w:t></w:r>"
        )),
        "{p}"
    );
}

#[test]
fn two_text_children_and_a_break_stay_in_one_run() {
    // t+t.docx
    let doc = load("<w:p><w:r><w:t>a</w:t><w:t>b</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!("<w:r>{T}a</w:t>{T}b</w:t></w:r>")),
        "{p}"
    );

    let doc = load("<w:p><w:r><w:t>a</w:t><w:br/><w:t>b</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!("<w:r>{T}a</w:t><w:br/>{T}b</w:t></w:r>")),
        "{p}"
    );
}

#[test]
fn hyphen_elements_next_to_a_tab_stay_in_one_run() {
    // BackgroundReport: a tab, then a non-breaking hyphen and its text.
    let doc =
        load("<w:p><w:r><w:rPr><w:i/></w:rPr><w:tab/><w:noBreakHyphen/><w:t>x</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert_eq!(run_count(&p), 1, "{p}");
    assert!(
        p.contains(&format!("<w:tab/><w:noBreakHyphen/>{T}x</w:t></w:r>")),
        "{p}"
    );

    // The hyphen joins "b" only: "a" and "b" still share the `w:r`.
    let doc = load("<w:p><w:r><w:t>a</w:t><w:t>b</w:t><w:noBreakHyphen/></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!(
            "<w:r>{T}a</w:t>{T}b</w:t><w:noBreakHyphen/></w:r>"
        )),
        "{p}"
    );
}

#[test]
fn last_rendered_page_break_survives_save() {
    let doc = load("<w:p><w:r><w:lastRenderedPageBreak/><w:t>x</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!("<w:r><w:lastRenderedPageBreak/>{T}x</w:t></w:r>")),
        "{p}"
    );

    let doc = load(
        "<w:p><w:r><w:t>a</w:t><w:lastRenderedPageBreak/><w:tab/>\
         <w:lastRenderedPageBreak/><w:br/><w:lastRenderedPageBreak/><w:t>b</w:t></w:r></w:p>",
    );
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!(
            "<w:r>{T}a</w:t><w:lastRenderedPageBreak/><w:tab/><w:lastRenderedPageBreak/>\
             <w:br/><w:lastRenderedPageBreak/>{T}b</w:t></w:r>"
        )),
        "{p}"
    );
}

#[test]
fn a_page_break_marker_keeps_its_place_between_text_and_a_hyphen() {
    // Without the marker the hyphen and the text would join one Run.
    let doc =
        load("<w:p><w:r><w:t>a</w:t><w:lastRenderedPageBreak/><w:noBreakHyphen/></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!(
            "<w:r>{T}a</w:t><w:lastRenderedPageBreak/><w:noBreakHyphen/></w:r>"
        )),
        "{p}"
    );

    let doc =
        load("<w:p><w:r><w:noBreakHyphen/><w:lastRenderedPageBreak/><w:t>b</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!(
            "<w:r><w:noBreakHyphen/><w:lastRenderedPageBreak/>{T}b</w:t></w:r>"
        )),
        "{p}"
    );
}

#[test]
fn an_explicit_text_wrapping_break_type_survives_save() {
    // chart*.docx
    let doc = load(
        "<w:p><w:r><w:br w:type=\"textWrapping\" w:clear=\"all\"/></w:r>\
         <w:r><w:br w:clear=\"all\"/></w:r><w:r><w:br w:type=\"textWrapping\"/></w:r>\
         <w:r><w:br/></w:r></w:p>",
    );
    let p = saved_para(&doc);
    assert!(
        p.contains(
            "<w:r><w:br w:type=\"textWrapping\" w:clear=\"all\"/></w:r>\
             <w:r><w:br w:clear=\"all\"/></w:r><w:r><w:br w:type=\"textWrapping\"/></w:r>\
             <w:r><w:br/></w:r>"
        ),
        "{p}"
    );
}

#[test]
fn separate_equal_runs_stay_separate() {
    let doc = load("<w:p><w:r><w:t>a</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>b</w:t></w:r></w:p>");
    let p = saved_para(&doc);
    assert_eq!(run_count(&p), 3, "{p}");
}

#[test]
fn the_model_is_the_same_as_for_separate_runs() {
    let grouped = load("<w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p>");
    let separate =
        load("<w:p><w:r><w:t>a</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>b</w:t></w:r></w:p>");
    // Same inlines, text and formatting: the shape is not formatting.
    assert_eq!(grouped, separate);
    // It is recorded, though, and an undo-style strict comparison sees it.
    assert!(!strictly_equal(&grouped, &separate));
    let (Block::Paragraph(g), Block::Paragraph(s)) = (&grouped.body[0], &separate.body[0]) else {
        unreachable!()
    };
    assert_eq!(g.content.len(), 3);
    assert_eq!(
        g.content.iter().map(Inline::text).collect::<String>(),
        s.content.iter().map(Inline::text).collect::<String>()
    );
}

#[test]
fn a_rewritten_hyperlink_keeps_its_run_grouping() {
    let mut doc = load(
        "<w:p><w:hyperlink w:anchor=\"x\"><w:r><w:t>a</w:t><w:t>b</w:t></w:r></w:hyperlink></w:p>",
    );
    let Block::Paragraph(p) = &mut doc.body[0] else {
        unreachable!()
    };
    let Inline::Hyperlink(h) = &mut p.content[0] else {
        panic!("{:?}", p.content)
    };
    h.content_changed = true;
    let p = saved_para(&doc);
    assert!(
        p.contains(&format!("<w:r>{T}a</w:t>{T}b</w:t></w:r></w:hyperlink>")),
        "{p}"
    );
}

const MARKED: &str = "<w:p><w:r><w:lastRenderedPageBreak/><w:t>abcdef</w:t></w:r></w:p>";

#[test]
fn bolding_the_middle_of_a_marked_run_keeps_one_marker() {
    let mut ed = Editor::new(load(MARKED));
    ed.anchor = Some(Caret::top(0, 2));
    ed.caret = Caret::top(0, 4);
    ed.toggle_bold();
    assert_eq!(markers(&ed.doc), 1);
    let p = saved_para(&ed.doc);
    assert!(
        p.contains(&format!("<w:lastRenderedPageBreak/>{T}ab</w:t>")),
        "{p}"
    );

    // Bolding the start: the bold piece is first and keeps it.
    let mut ed = Editor::new(load(MARKED));
    ed.anchor = Some(Caret::top(0, 0));
    ed.caret = Caret::top(0, 2);
    ed.toggle_bold();
    assert_eq!(markers(&ed.doc), 1);
    let p = saved_para(&ed.doc);
    assert!(
        p.contains(&format!("<w:lastRenderedPageBreak/>{T}ab</w:t>")),
        "{p}"
    );
}

#[test]
fn enter_in_a_marked_run_keeps_one_marker() {
    let mut ed = Editor::new(load(MARKED));
    ed.set_caret(Caret::top(0, 3));
    ed.insert_newline();
    assert_eq!(saved_paras(&ed.doc).len(), 2);
    assert_eq!(markers(&ed.doc), 1);
}

#[test]
fn typing_after_a_marked_tab_keeps_one_marker() {
    let mut ed = Editor::new(load(
        "<w:p><w:r><w:t>a</w:t><w:lastRenderedPageBreak/><w:tab/></w:r></w:p>",
    ));
    ed.set_caret(Caret::top(0, 2));
    ed.insert_char('x');
    assert_eq!(markers(&ed.doc), 1);
    let p = saved_para(&ed.doc);
    assert!(
        p.contains(&format!("<w:lastRenderedPageBreak/><w:tab/>{T}x</w:t>")),
        "{p}"
    );
}

#[test]
fn an_edit_elsewhere_keeps_an_untouched_paragraphs_grouping() {
    let mut ed = Editor::new(load(
        "<w:p><w:r><w:t>first</w:t></w:r></w:p>\
         <w:p><w:r><w:lastRenderedPageBreak/><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p>",
    ));
    ed.set_caret(Caret::top(0, 5));
    ed.insert_char('!');
    let paras = saved_paras(&ed.doc);
    assert!(paras[0].contains("first!"), "{}", paras[0]);
    assert!(
        paras[1].contains(&format!(
            "<w:r><w:lastRenderedPageBreak/>{T}a</w:t><w:tab/>{T}b</w:t></w:r>"
        )),
        "{}",
        paras[1]
    );
}
