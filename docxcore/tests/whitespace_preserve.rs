//! `w:t` / `w:delText` text is the text Word reads (#1084): without
//! `xml:space="preserve"` on the element or an ancestor, the XML whitespace
//! (space, tab, CR, LF) at its two ends is not significant. So
//! `<w:t>SDT </w:t><w:t> Run</w:t>` is "SDTRun", in the editor and after a
//! save that writes every `w:t` with `preserve`.

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml, parse_header_footer};
use docxcore::model::{Block, Document, Inline};
use docxcore::package::{load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn load(body: &str) -> Document {
    parse_document_xml(
        &format!("<w:document xmlns:w=\"{W_NS}\"><w:body>{body}</w:body></w:document>"),
        &Relationships::default(),
    )
}

fn text(body: &str) -> String {
    match &load(body).body[0] {
        Block::Paragraph(p) => p.plain_text(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

/// The text of every run of the first paragraph, one entry per Run.
fn run_texts(doc: &Document) -> Vec<String> {
    match &doc.body[0] {
        Block::Paragraph(p) => p
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some(r.text.clone()),
                _ => None,
            })
            .collect(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

#[test]
fn unpreserved_edge_whitespace_is_not_text() {
    assert_eq!(
        text("<w:p><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r></w:p>"),
        "SDTRun"
    );
    assert_eq!(
        text(
            "<w:p><w:r><w:t xml:space=\"preserve\">SDT </w:t></w:r>\
             <w:r><w:t>Run</w:t></w:r></w:p>"
        ),
        "SDT Run"
    );
}

#[test]
fn only_xml_whitespace_at_the_two_ends_is_dropped() {
    // Tab, CR and LF at the ends; interior runs of spaces stay.
    assert_eq!(
        text("<w:p><w:r><w:t>\t\r\n A  B \n\t</w:t></w:r></w:p>"),
        "A  B"
    );
    // NBSP is text, not XML whitespace.
    assert_eq!(
        text("<w:p><w:r><w:t>\u{a0}A\u{a0}</w:t></w:r></w:p>"),
        "\u{a0}A\u{a0}"
    );
    // All whitespace: an empty Run.
    let doc = load("<w:p><w:r><w:t> \t </w:t></w:r><w:r><w:t>x</w:t></w:r></w:p>");
    assert_eq!(run_texts(&doc), ["", "x"]);
}

#[test]
fn deleted_text_follows_the_same_rule() {
    assert_eq!(
        text(
            "<w:p><w:del w:id=\"1\" w:author=\"A\"><w:r><w:delText>gone </w:delText></w:r>\
             <w:r><w:delText xml:space=\"preserve\"> kept</w:delText></w:r></w:del></w:p>"
        ),
        "gone kept"
    );
}

#[test]
fn an_ancestors_preserve_applies_and_default_cancels_it() {
    // On the run, the paragraph, a hyperlink and a content control.
    assert_eq!(
        text(
            "<w:p><w:r xml:space=\"preserve\"><w:t>SDT </w:t></w:r><w:r><w:t>Run</w:t></w:r></w:p>"
        ),
        "SDT Run"
    );
    assert_eq!(
        text(
            "<w:p xml:space=\"preserve\"><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r></w:p>"
        ),
        "SDT  Run"
    );
    assert_eq!(
        text(
            "<w:p><w:hyperlink w:anchor=\"a\" xml:space=\"preserve\"><w:r><w:t>SDT </w:t></w:r>\
             </w:hyperlink><w:r><w:t>Run</w:t></w:r></w:p>"
        ),
        "SDT Run"
    );
    assert_eq!(
        text(
            "<w:p><w:sdt><w:sdtPr/><w:sdtContent xml:space=\"preserve\"><w:r><w:t>SDT </w:t></w:r>\
             </w:sdtContent></w:sdt><w:r><w:t> Run</w:t></w:r></w:p>"
        ),
        "SDT Run"
    );
    // The nearest `xml:space` wins.
    assert_eq!(
        text(
            "<w:p xml:space=\"preserve\"><w:r xml:space=\"default\"><w:t>SDT </w:t></w:r>\
             <w:r><w:t xml:space=\"default\"> Run</w:t></w:r><w:r><w:t> !</w:t></w:r></w:p>"
        ),
        "SDTRun !"
    );
    // On the document root, too.
    let doc = parse_document_xml(
        &format!(
            "<w:document xmlns:w=\"{W_NS}\" xml:space=\"preserve\"><w:body>\
             <w:p><w:r><w:t>SDT </w:t></w:r></w:p></w:body></w:document>"
        ),
        &Relationships::default(),
    );
    assert_eq!(run_texts(&doc), ["SDT "]);
}

#[test]
fn headers_and_footers_read_text_the_same_way() {
    let blocks = parse_header_footer(
        &format!(
            "<w:hdr xmlns:w=\"{W_NS}\"><w:p><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r>\
             </w:p></w:hdr>"
        ),
        &Relationships::default(),
    );
    assert_eq!(blocks[0].plain_text(), "SDTRun");
}

fn package(body: &str) -> Vec<u8> {
    let part = |n: &str, b: String| (n.to_string(), b.into_bytes());
    write_zip(&[
        part(
            "[Content_Types].xml",
            "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
             </Types>"
                .into(),
        ),
        part(
            "_rels/.rels",
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
             </Relationships>"
                .into(),
        ),
        part(
            "word/document.xml",
            format!("<w:document xmlns:w=\"{W_NS}\"><w:body>{body}</w:body></w:document>"),
        ),
    ])
}

/// The save half: once anything is edited the body is written from the
/// model with `preserve` on every `w:t`, so the model must hold Word's text,
/// or the untouched "SDT " / " Run" would show as "SDT  Run" in Word.
#[test]
fn an_edited_save_writes_the_text_word_read() {
    let original = package(
        "<w:p><w:r><w:t>SDT </w:t></w:r><w:r><w:t> Run</w:t></w:r></w:p>\
         <w:p><w:r><w:t>other</w:t></w:r></w:p>",
    );
    let mut pkg = load_package(&original).unwrap();
    let mut ed = Editor::new(pkg.document.clone());
    ed.set_caret(Caret::at(vec![1], 0));
    ed.insert_char('x');
    pkg.document = ed.doc;
    let saved = save_package(&pkg);

    let xml = String::from_utf8(
        ZipArchive::open(&saved)
            .unwrap()
            .read("word/document.xml")
            .unwrap(),
    )
    .unwrap();
    let reloaded = load_package(&saved).unwrap().document;
    assert_eq!(run_texts(&reloaded), ["SDT", "Run"], "{xml}");
    assert_eq!(reloaded.body[0].plain_text(), "SDTRun");
    assert_eq!(reloaded.body[1].plain_text(), "xother");
    // Every saved `w:t` text of the first paragraph is free of edge spaces,
    // so its `preserve` changes nothing Word shows.
    let first = &xml[..xml.find("</w:p>").unwrap()];
    for piece in first.split("<w:t xml:space=\"preserve\">").skip(1) {
        let t = &piece[..piece.find("</w:t>").unwrap()];
        assert_eq!(t, t.trim(), "{xml}");
    }
}
