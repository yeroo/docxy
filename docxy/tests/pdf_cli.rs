//! `docxy file.docx --pdf out.pdf` honours the document's page breaks (#637).

use std::path::Path;
use std::process::Command;

use docxcore::zipwrite::write_zip;

/// "Before", a page break, "After" — Word's Insert > Page Break.
fn breaks_docx() -> Vec<u8> {
    docx_with("Before")
}

/// [`breaks_docx`] with `first` on the first page.
fn docx_with(first: &str) -> Vec<u8> {
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
            document.replace("Before", first).into_bytes(),
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
        // No fonts: the plain Courier file, the same on every machine.
        .env("DOCXY_PDF_FONT_DIR", dir.join("no-fonts"))
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

#[path = "../../docxcore/tests/common/pdf_read.rs"]
mod pdf_read;
#[path = "../../docxcore/tests/common/synth_font.rs"]
mod synth_font;

#[test]
fn fonts_under_the_font_dir_are_embedded_and_copy_back_as_text() {
    let dir = std::env::temp_dir().join(format!("docxy-pdf-cli-fonts-{}", std::process::id()));
    let fonts = dir.join("fonts");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&fonts).unwrap();
    let glyphs: Vec<_> = "Before After Привет 中文"
        .chars()
        .map(|c| synth_font::glyph(c, 600))
        .collect();
    let style = synth_font::Style {
        family: Some("Arial".into()),
        ..Default::default()
    };
    std::fs::write(
        fonts.join("a.ttf"),
        synth_font::build_with(&glyphs, 1000, 0, &style),
    )
    .unwrap();
    let docx = dir.join("in.docx");
    std::fs::write(&docx, docx_with("Привет 中文 Q")).unwrap();
    let pdf = dir.join("out.pdf");
    let out = Command::new(env!("CARGO_BIN_EXE_docxy"))
        .args([docx.as_path(), Path::new("--pdf"), pdf.as_path()])
        .env("DOCXY_PDF_FONT_DIR", &fonts)
        .output()
        .expect("run docxy");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/Subtype /Type0"));
    assert_eq!(pdf_read::text_of(&bytes), "Привет中文QAfter");
    let _ = std::fs::remove_dir_all(&dir);
}
