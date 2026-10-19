//! #920: a Word TOC entry (an internal link holding a tab and no PAGEREF
//! field) stays one `w:hyperlink` around its tab, loaded, edited and saved.

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline};
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const ENTRY: &str = "<w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>Intro</w:t></w:r>\
    <w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r>\
    <w:r><w:rPr><w:webHidden/></w:rPr><w:t>3</w:t></w:r></w:hyperlink>";

fn load() -> Document {
    let xml =
        format!("<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>{ENTRY}</w:p></w:body></w:document>");
    parse_document_xml(&xml, &Relationships::default())
}

fn text(doc: &Document) -> String {
    let mut ed = Editor::new(doc.clone());
    ed.select_all();
    ed.selection_text()
}

fn one_link_with_tab(xml: &str) {
    assert_eq!(xml.matches("<w:hyperlink").count(), 1, "{xml}");
    assert!(!xml.contains("w:vanish"), "{xml}");
    assert_eq!(xml.matches("<w:webHidden/>").count(), 2, "{xml}");
    let (open, close) = (
        xml.find("<w:hyperlink").unwrap(),
        xml.find("</w:hyperlink>"),
    );
    let tab = xml.find("<w:tab/>").unwrap();
    assert!(open < tab && tab < close.unwrap(), "{xml}");
}

#[test]
fn loads_as_one_link_holding_its_tab() {
    let doc = load();
    let Block::Paragraph(p) = &doc.body[0] else {
        panic!("no paragraph")
    };
    assert_eq!(p.content.len(), 1, "{:?}", p.content);
    let Inline::Hyperlink(h) = &p.content[0] else {
        panic!("expected a link, got {:?}", p.content[0])
    };
    assert_eq!(h.anchor.as_deref(), Some("_Toc1"));
    assert!(h.runs.is_empty());
    assert!(h.content.iter().any(|i| matches!(i, Inline::Tab(_))));
    assert_eq!(text(&doc), "Intro\t3");
}

#[test]
fn untouched_save_is_byte_faithful() {
    let doc = load();
    let xml = document_to_xml(&doc);
    assert!(xml.contains(ENTRY), "{xml}");
    one_link_with_tab(&xml);
}

#[test]
fn typing_in_the_link_keeps_one_link_around_the_tab() {
    let mut ed = Editor::new(load());
    ed.set_caret(Caret::at(vec![0], 2));
    ed.insert_char('x');
    assert_eq!(text(&ed.doc), "Inxtro\t3");
    one_link_with_tab(&document_to_xml(&ed.doc));
}

#[test]
fn deleting_the_tab_keeps_the_link() {
    let mut ed = Editor::new(load());
    ed.set_caret(Caret::at(vec![0], 6));
    ed.backspace();
    assert_eq!(text(&ed.doc), "Intro3");
    let xml = document_to_xml(&ed.doc);
    assert_eq!(xml.matches("<w:hyperlink").count(), 1, "{xml}");
    assert!(
        !xml.contains("<w:tab/>") && !xml.contains("w:vanish"),
        "{xml}"
    );

    let mut ed = Editor::new(load());
    ed.set_caret(Caret::at(vec![0], 5));
    ed.delete_forward();
    assert_eq!(text(&ed.doc), "Intro3");
}

#[test]
fn enter_inside_the_link_splits_it_without_losing_the_tab() {
    let mut ed = Editor::new(load());
    ed.set_caret(Caret::at(vec![0], 3));
    ed.insert_newline();
    assert_eq!(text(&ed.doc), "Int\nro\t3");
    let xml = document_to_xml(&ed.doc);
    assert_eq!(xml.matches("<w:tab/>").count(), 1, "{xml}");
    assert_eq!(xml.matches("w:anchor=\"_Toc1\"").count(), 2, "{xml}");
    assert!(!xml.contains("w:vanish"), "{xml}");
}
