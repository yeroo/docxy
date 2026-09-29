//! `docxy file.docx --pdf out.pdf` honours the document's page breaks (#637).

use std::path::Path;
use std::process::Command;

use docxcore::zipwrite::write_zip;

/// "Before", a page break, "After" — Word's Insert > Page Break.
fn breaks_docx() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Before</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/></w:r><w:r><w:t>After</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#;
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            document.as_bytes().to_vec(),
        ),
    ])
}

#[test]
fn a_page_break_gives_a_two_page_pdf() {
    let dir = std::env::temp_dir().join(format!("docxy-pdf-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let docx = dir.join("breaks.docx");
    std::fs::write(&docx, breaks_docx()).unwrap();
    let pdf = dir.join("out.pdf");
    let out = Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args([docx.as_path(), Path::new("--pdf"), pdf.as_path()])
        .output()
        .expect("run docxy");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&std::fs::read(&pdf).unwrap()).into_owned();
    assert!(text.contains("/Count 2 "), "expected 2 pages");
    let before = text.find("(Before)").unwrap();
    let after = text.find("(After)").unwrap();
    // Each page's content stream precedes its page object.
    let first_page = text.find("/Type /Page /Parent").unwrap();
    assert!(
        before < first_page && first_page < after,
        "After is on page 2"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
