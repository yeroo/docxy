//! PDF export end to end: a `.docx` with a page break, a landscape section, a
//! header and a footer PAGE field, loaded and printed through
//! `PdfOptions::from_package` (#637).

use std::rc::Rc;

use docxcore::export::{PdfOptions, to_pdf};
use docxcore::package::load_package;
use docxcore::styles::StyleSheet;
use docxcore::zipwrite::write_zip;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn docx() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/></Types>"#;
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let document_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH" Type="{REL}/header" Target="header1.xml"/><Relationship Id="rIdF" Type="{REL}/footer" Target="footer1.xml"/><Relationship Id="rIdS" Type="{REL}/settings" Target="settings.xml"/></Relationships>"#
    );
    // Page 1 "Before", a page break, page 2 "After" (still portrait); a Next
    // Page section break, then page 3 in landscape. Both sections share the
    // header by linking to the previous section.
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}"><w:background w:color="FFFFCC"/><w:body>
<w:p><w:r><w:t>Before</w:t></w:r><w:r><w:br w:type="page"/></w:r></w:p>
<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdH"/><w:footerReference w:type="default" r:id="rIdF"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:pPr><w:r><w:t>After</w:t></w:r></w:p>
<w:p><w:r><w:t>Wide</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>
</w:body></w:document>"#
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" xmlns:r="{R}"><w:p><w:r><w:t>Running head</w:t></w:r></w:p></w:hdr>"#
    );
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W}" xmlns:r="{R}"><w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>"#
    );
    let settings = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="{W}"><w:mirrorMargins/></w:settings>"#
    );
    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.into_bytes()),
        ("word/document.xml".to_string(), document.into_bytes()),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels.into_bytes(),
        ),
        ("word/header1.xml".to_string(), header.into_bytes()),
        ("word/footer1.xml".to_string(), footer.into_bytes()),
        ("word/settings.xml".to_string(), settings.into_bytes()),
    ])
}

/// Each page's MediaBox and content stream, in page order.
fn pages(pdf: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(pdf);
    let mut out = Vec::new();
    let mut rest: &str = &text;
    let mut last_stream = String::new();
    while let Some(i) = rest.find(" 0 obj\n") {
        rest = &rest[i + 7..];
        let end = rest.find("\nendobj\n").unwrap();
        let body = &rest[..end];
        if let Some(s) = body.find("stream\n") {
            last_stream = body[s + 7..body.rfind("\nendstream").unwrap()].to_string();
        } else if body.starts_with("<< /Type /Page /Parent") {
            let media = body.split("/MediaBox [").nth(1).unwrap();
            out.push((
                media[..media.find(']').unwrap()].to_string(),
                last_stream.clone(),
            ));
        }
        rest = &rest[end..];
    }
    out
}

#[test]
fn a_loaded_docx_prints_its_breaks_sections_and_headers() {
    let pkg = load_package(&docx()).expect("load");
    let opts = PdfOptions::from_package(&pkg, Rc::new(StyleSheet::default()));
    assert!(opts.mirror_margins, "settings flag read");
    assert_eq!(opts.background, Some((0xFF, 0xFF, 0xCC)));
    let pdf = to_pdf(&pkg.document, &opts);
    let pages = pages(&pdf);
    assert_eq!(pages.len(), 3, "page break + next-page section");
    assert_eq!(pages[0].0, "0 0 612.00 792.00");
    assert_eq!(pages[1].0, "0 0 612.00 792.00");
    assert_eq!(pages[2].0, "0 0 792.00 612.00", "landscape section");
    assert!(pages[0].1.contains("(Before)") && !pages[0].1.contains("(After)"));
    assert!(pages[1].1.contains("(After)"));
    assert!(pages[2].1.contains("(Wide)"));
    for (i, (_, content)) in pages.iter().enumerate() {
        assert!(
            content.contains("(Running head)"),
            "header on page {}",
            i + 1
        );
        assert!(
            content.contains(&format!("({}) Tj", i + 1)),
            "footer PAGE field on page {}: {content}",
            i + 1
        );
        assert!(content.contains("re f"), "page colour on page {}", i + 1);
    }
}

#[test]
fn from_package_on_a_plain_document_keeps_the_default_page() {
    let pkg = docxcore::package::new_package(docxcore::model::Document::default());
    let opts = PdfOptions::from_package(&pkg, Rc::new(StyleSheet::default()));
    let pages = pages(&to_pdf(&pkg.document, &opts));
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].0, "0 0 612.00 792.00");
}
