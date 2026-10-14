//! #1242: an equation and an inline picture are one editor offset each, on a
//! real package: `ab<object>cd` loaded from `.docx` bytes, edited through the
//! `Editor`, saved, and checked in `word/document.xml`.

use docxcore::editor::{Caret, Editor};
use docxcore::package::{Package, load_package, save_package};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

/// An inline picture as Word writes it (Insert > Pictures), media and all.
const PICTURE: &str = "<w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
    <wp:extent cx=\"952500\" cy=\"952500\"/><wp:docPr id=\"1\" name=\"Picture 1\"/>\
    <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
    <pic:pic><pic:nvPicPr><pic:cNvPr id=\"0\" name=\"image1.png\"/><pic:cNvPicPr/></pic:nvPicPr>\
    <pic:blipFill><a:blip r:embed=\"rId5\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
    <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"952500\" cy=\"952500\"/></a:xfrm>\
    <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData>\
    </a:graphic></wp:inline></w:drawing></w:r>";

/// An inline equation (Insert > Equation): x+1.
const EQUATION: &str = "<m:oMath><m:r><m:t>x+1</m:t></m:r></m:oMath>";

/// A package whose one paragraph is `ab<object>cd`.
fn docx(object: &str) -> Vec<u8> {
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" \
         xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\">\
         <w:body><w:p><w:r><w:t>ab</w:t></w:r>{object}<w:r><w:t>cd</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr></w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Default Extension=\"png\" ContentType=\"image/png\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
        </Relationships>";
    let doc_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId5\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.png\"/>\
        </Relationships>";
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        ("word/document.xml".to_string(), document.into_bytes()),
        (
            "word/_rels/document.xml.rels".to_string(),
            doc_rels.as_bytes().to_vec(),
        ),
        (
            "word/media/image1.png".to_string(),
            b"\x89PNG\r\n\x1a\n".to_vec(),
        ),
    ])
}

fn open(object: &str) -> (Package, Editor) {
    let pkg = load_package(&docx(object)).expect("load the package");
    let editor = Editor::new(pkg.document.clone());
    (pkg, editor)
}

/// Ctrl+S: the saved paragraph, between `<w:p>` and `</w:p>`.
fn saved(pkg: &Package, editor: &Editor) -> String {
    let mut pkg = pkg.clone();
    pkg.document = editor.doc.clone();
    let bytes = save_package(&pkg);
    let zip = ZipArchive::open(&bytes).expect("saved zip");
    let xml = String::from_utf8(zip.read("word/document.xml").expect("document part")).unwrap();
    let start = xml.find("<w:p>").or_else(|| xml.find("<w:p ")).unwrap();
    let end = xml[start..].find("</w:p>").unwrap() + start;
    xml[start..end].to_string()
}

#[test]
fn backspace_or_delete_removes_an_inline_picture_at_once() {
    for (at, delete) in [(3, false), (2, true)] {
        let (pkg, mut ed) = open(PICTURE);
        ed.set_caret(Caret::at(vec![0], at));
        if delete {
            ed.delete_forward();
        } else {
            ed.backspace();
        }
        assert!(!ed.has_selection(), "a picture is not selected first");
        let p = saved(&pkg, &ed);
        assert!(!p.contains("<w:drawing"), "{p}");
        assert!(p.contains(">ab<") && p.contains(">cd<"), "{p}");
        assert!(ed.undo());
        assert!(
            saved(&pkg, &ed).contains("<pic:cNvPr id=\"0\" name=\"image1.png\"/>"),
            "undo brings the picture back"
        );
        assert_eq!(saved(&pkg, &ed), saved(&pkg, &open(PICTURE).1));
    }
}

#[test]
fn backspace_or_delete_selects_an_equation_then_removes_it() {
    for (at, delete) in [(3, false), (2, true)] {
        let (pkg, mut ed) = open(EQUATION);
        let press = |ed: &mut Editor| {
            if delete {
                ed.delete_forward();
            } else {
                ed.backspace();
            }
        };
        ed.set_caret(Caret::at(vec![0], at));
        press(&mut ed);
        assert!(ed.has_selection(), "the first press selects the equation");
        assert!(saved(&pkg, &ed).contains("<m:oMath>"));
        press(&mut ed);
        let p = saved(&pkg, &ed);
        assert!(!p.contains("m:oMath"), "{p}");
        assert!(ed.undo());
        assert_eq!(saved(&pkg, &ed), saved(&pkg, &open(EQUATION).1));
    }
}

#[test]
fn typing_goes_before_or_after_the_object_never_into_it() {
    for (object, open_tag, close_tag) in [
        (PICTURE, "<w:drawing>", "</w:drawing>"),
        (EQUATION, "<m:oMath>", "</m:oMath>"),
    ] {
        let (pkg, mut ed) = open(object);
        ed.set_caret(Caret::at(vec![0], 2));
        ed.insert_char('X');
        let p = saved(&pkg, &ed);
        assert!(p.contains(">abX<"), "{p}");
        assert!(p.find(">abX<").unwrap() < p.find(open_tag).unwrap(), "{p}");

        let (pkg, mut ed) = open(object);
        ed.set_caret(Caret::at(vec![0], 3));
        ed.insert_char('X');
        let p = saved(&pkg, &ed);
        let (a, b) = (p.find(open_tag).unwrap(), p.find(close_tag).unwrap());
        assert!(!p[a..b].contains('X'), "{p}");
        assert!(p.find(">Xcd<").unwrap() > b, "X after the object: {p}");
    }
}
