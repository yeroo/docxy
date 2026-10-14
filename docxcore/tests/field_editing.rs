//! #642: a field is edited as one unit. Both repros from the issue, run on a
//! real package: "Body" then a complex PAGE field, loaded from `.docx` bytes,
//! edited through the `Editor`, saved, and checked in `word/document.xml`.
//!
//! #1240: a TOC whose begin and end sit in different paragraphs, which #642
//! leaves as loose markers, is one unit at its edges too.

use docxcore::editor::{Caret, Clip, Editor};
use docxcore::model::{Block, Inline};
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
    docx(&format!(
        "<w:p><w:r><w:t>Body</w:t></w:r>{PAGE_FIELD}</w:p>"
    ))
}

/// The `word/document.xml` of a package whose body is `body`.
fn document_xml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr></w:body></w:document>"
    )
}

/// A package whose body is `body`.
fn docx(body: &str) -> Vec<u8> {
    let document = document_xml(body);
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
    open_bytes(&f_docx())
}

fn open_bytes(bytes: &[u8]) -> (Package, Editor) {
    let pkg = load_package(bytes).expect("load the package");
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

/// The TOC field's begin, instruction and separate, as Word writes them.
const TOC_START: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
    <w:r><w:instrText xml:space=\"preserve\"> TOC \\o \"1-3\" \\h \\z \\u </w:instrText></w:r>\
    <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>";

/// One TOC entry's paragraph: a link to its heading, holding the title, a
/// tab and the page number; `start` goes first in it.
fn toc_entry(start: &str, n: u32, title: &str) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"TOC1\"/></w:pPr>{start}\
         <w:hyperlink w:anchor=\"_Toc{n}\" w:history=\"1\">\
         <w:r><w:t>{title}</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>{n}</w:t></w:r>\
         </w:hyperlink></w:p>"
    )
}

/// A table of contents as Word inserts it (References > Table of Contents):
/// a docPart content control holding the entries, the TOC field's begin in
/// the first entry's paragraph and its end in a paragraph of its own.
fn toc_body() -> String {
    format!(
        "<w:p><w:r><w:t>Before</w:t></w:r></w:p>\
         <w:sdt><w:sdtPr><w:docPartObj><w:docPartGallery w:val=\"Table of Contents\"/>\
         <w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>\
         {}{}{}<w:p><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p>\
         </w:sdtContent></w:sdt>\
         <w:p><w:r><w:t>After</w:t></w:r></w:p>",
        toc_entry(TOC_START, 1, "Introduction"),
        toc_entry("", 2, "Background"),
        toc_entry("", 3, "Results"),
    )
}

/// The caret just after the TOC field's end: the start of the paragraph
/// holding its end run.
fn after_toc(ed: &Editor) -> Caret {
    let at = ed
        .doc
        .body
        .iter()
        .position(|b| {
            matches!(b, Block::Paragraph(p) if p.content.iter().any(
                |i| matches!(i, Inline::Raw(r) if r.contains("fldCharType=\"end\""))))
        })
        .expect("the end run's paragraph");
    Caret::at(vec![at], 0)
}

#[test]
fn toc_spanning_paragraphs_deletes_as_one_unit_1240() {
    let (pkg, mut ed) = open_bytes(&docx(&toc_body()));
    // A guard: unedited, the TOC keeps its field through a save, and saves
    // the same again once reloaded. (Save writes the part afresh, so it is
    // not the source's bytes: a link holding a tab, say, saves as two.)
    let untouched = save(pkg.clone(), &ed);
    for kind in ["begin", "separate", "end"] {
        let marker = format!("w:fldCharType=\"{kind}\"");
        assert_eq!(untouched.matches(&marker).count(), 1, "{kind}: {untouched}");
    }
    let mut reloaded = pkg.clone();
    reloaded.document =
        docxcore::load::parse_document_xml(&untouched, &docxcore::load::Relationships::default());
    let again = Editor::new(reloaded.document.clone());
    assert_eq!(save(reloaded, &again), untouched);

    let e = after_toc(&ed);
    ed.set_caret(e.clone());
    ed.backspace();
    let (lo, hi) = ed
        .selection_range()
        .expect("the first Backspace selects the TOC");
    assert_eq!((lo.offset, hi), (0, e));
    assert_eq!(
        save(pkg.clone(), &ed),
        untouched,
        "selecting changes nothing"
    );

    ed.backspace();
    let saved = save(pkg.clone(), &ed);
    for gone in ["fldChar", "instrText", "TOC \\o", "Introduction", "Results"] {
        assert!(!saved.contains(gone), "{gone} left behind: {saved}");
    }
    assert!(saved.contains(">Before<") && saved.contains(">After<"));

    assert!(ed.undo());
    assert_eq!(save(pkg, &ed), untouched, "one undo brings the TOC back");
}
