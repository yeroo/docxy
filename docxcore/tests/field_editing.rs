//! #642: a field is edited as one unit. Both repros from the issue, run on a
//! real package: "Body" then a complex PAGE field, loaded from `.docx` bytes,
//! edited through the `Editor`, saved, and checked in `word/document.xml`.

use docxcore::editor::{Caret, Clip, Editor};
use docxcore::model::Inline;
use docxcore::package::{Package, load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

/// The PAGE field as Word writes it (Insert > Quick Parts > Field > Page).
const PAGE_FIELD: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
    <w:r><w:instrText xml:space=\"preserve\"> PAGE   \\* MERGEFORMAT </w:instrText></w:r>\
    <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
    <w:r><w:rPr><w:noProof/></w:rPr><w:t>1</w:t></w:r>\
    <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";

fn f_docx() -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body><w:p><w:r><w:t>Body</w:t></w:r>{PAGE_FIELD}</w:p>\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr></w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document.into_bytes()),
    ])
}

fn open() -> (Package, Editor) {
    let pkg = load_package(&f_docx()).expect("load f.docx");
    let editor = Editor::new(pkg.document.clone());
    (pkg, editor)
}

/// Ctrl+S: the saved `word/document.xml`.
fn save(mut pkg: Package, editor: &Editor) -> String {
    pkg.document = editor.doc.clone();
    let bytes = save_package(&pkg);
    let zip = ZipArchive::open(&bytes).expect("saved zip");
    let xml = zip.read("word/document.xml").expect("document part");
    String::from_utf8(xml).expect("utf-8")
}

/// The saved paragraph, between `<w:p>` and `</w:p>`.
fn paragraph(xml: &str) -> &str {
    let start = xml.find("<w:p>").or_else(|| xml.find("<w:p ")).unwrap();
    let end = xml[start..].find("</w:p>").unwrap() + start;
    &xml[start..end]
}

#[test]
fn backspace_after_a_page_field_selects_then_deletes_the_whole_field() {
    let (pkg, mut ed) = open();
    ed.set_caret(Caret::at(vec![0], 5)); // end of the paragraph
    ed.backspace();
    assert!(ed.has_selection(), "the first Backspace selects the field");
    let untouched = save(pkg.clone(), &ed);
    assert!(paragraph(&untouched).contains(PAGE_FIELD));

    ed.backspace();
    let saved = save(pkg.clone(), &ed);
    let p = paragraph(&saved);
    for gone in ["fldChar", "instrText", "PAGE", ">1<"] {
        assert!(!p.contains(gone), "{gone} left behind: {p}");
    }
    assert!(p.contains(">Body<"));

    ed.backspace();
    let saved = save(pkg, &ed);
    assert!(paragraph(&saved).contains(">Bod<"), "then `y` goes");
}

#[test]
fn a_page_number_inserted_between_text_and_field_goes_before_the_field() {
    let (pkg, mut ed) = open();
    ed.set_caret(Caret::at(vec![0], 4)); // between "Body" and the field
    // Insert > Page Number: the app pastes a simple PAGE field.
    ed.paste(&Clip {
        paras: vec![vec![Inline::Field {
            raw: "<w:fldSimple w:instr=\"PAGE\"><w:r><w:t xml:space=\"preserve\">1</w:t></w:r></w:fldSimple>"
                .to_string(),
            text: "1".to_string(),
        }]],
    });
    let saved = save(pkg, &ed);
    let p = paragraph(&saved);
    let simple = p.find("<w:fldSimple").expect("the new field is saved");
    let begin = p
        .find("fldCharType=\"begin\"")
        .expect("the old field is kept");
    assert!(simple < begin, "new field before the old one's begin: {p}");
    assert!(p.contains(PAGE_FIELD), "the old field is intact: {p}");
    // And the editor shows both.
    let (lines, _) = docxcore::render::render_mapped(
        &ed.doc,
        &docxcore::render::RenderOptions {
            width: 40,
            ..Default::default()
        },
    );
    assert_eq!(lines[0].plain(), "Body11");
}
