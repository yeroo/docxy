//! `docxy env.docx --docx out.docx` keeps a Word envelope's framed delivery
//! address (#628): `<w:framePr w:wrap="auto"/>` used to come back as an
//! empty `<w:framePr/>`.

use std::path::Path;
use std::process::Command;

use docxcore::package::load_package;
use opccore::zipwrite::write_zip;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A two-section envelope document as `ActiveDocument.Envelope.Insert`
/// writes it: the envelope section (return address, framed delivery
/// address) and the letter.
fn envelope_docx() -> Vec<u8> {
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    let rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"word/document.xml\"/>\
         </Relationships>"
    );
    let doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>\
         <w:p><w:r><w:t>Return Address</w:t></w:r></w:p>\
         <w:p><w:pPr><w:framePr w:wrap=\"auto\"/></w:pPr><w:r><w:t>Jane Doe</w:t></w:r></w:p>\
         <w:p><w:pPr><w:framePr w:wrap=\"auto\"/>\
         <w:sectPr><w:pgSz w:w=\"13680\" w:h=\"5940\" w:orient=\"landscape\"/></w:sectPr>\
         </w:pPr><w:r><w:t>1 Main St</w:t></w:r></w:p>\
         <w:p><w:r><w:t>Dear Jane,</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>\
         </w:body></w:document>"
    );
    write_zip(&[
        ("[Content_Types].xml".into(), ct.as_bytes().to_vec()),
        ("_rels/.rels".into(), rels.into_bytes()),
        ("word/document.xml".into(), doc.into_bytes()),
    ])
}

#[test]
fn docx_round_trip_keeps_frame_wrap_auto() {
    let dir = std::env::temp_dir().join(format!("docxy-frame-pr-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("env.docx");
    let out = dir.join("out.docx");
    std::fs::write(&src, envelope_docx()).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args([src.as_path(), Path::new("--docx"), out.as_path()])
        .output()
        .expect("run docxy");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let pkg = load_package(&std::fs::read(&out).unwrap()).expect("output loads");
    let xml = pkg.part_text("word/document.xml").unwrap();
    assert_eq!(
        xml.matches("<w:framePr w:wrap=\"auto\"/>").count(),
        2,
        "both delivery-address paragraphs keep their frame: {xml}"
    );
    assert!(!xml.contains("<w:framePr/>"), "{xml}");
    assert!(xml.contains("w:w=\"13680\""), "{xml}");
}
