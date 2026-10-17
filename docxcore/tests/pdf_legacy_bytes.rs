//! The Courier/WinAnsi PDF path is the contract of every host without a font
//! provider (docxwasm, a machine with no fonts): its bytes must not move when
//! the layout code changes (#1508). Each hash below was taken from the output
//! before layout moved from character cells to points.

use std::rc::Rc;

use docxcore::export::{PdfOptions, to_pdf};
use docxcore::package::load_package;
use docxcore::styles::StyleSheet;
use docxcore::zipwrite::write_zip;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    h
}

/// A document with a header (text and a watermark), a footer PAGE field and
/// the given body.
fn docx(body: &str) -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let root_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
    );
    let document_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdH" Type="{REL}/header" Target="header1.xml"/><Relationship Id="rIdF" Type="{REL}/footer" Target="footer1.xml"/><Relationship Id="rIdL" Type="{REL}/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#
    );
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>{body}<w:sectPr><w:headerReference w:type="default" r:id="rIdH"/><w:footerReference w:type="default" r:id="rIdF"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W}" xmlns:r="{R}"><w:p><w:r><w:t>Running head</w:t></w:r></w:p></w:hdr>"#
    );
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr xmlns:w="{W}" xmlns:r="{R}"><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>"#
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
    ])
}

fn pdf_of(bytes: &[u8]) -> Vec<u8> {
    let pkg = load_package(bytes).expect("load");
    let opts = PdfOptions::from_package(&pkg, Rc::new(StyleSheet::default()));
    to_pdf(&pkg.document, &opts)
}

fn para(text: &str) -> String {
    format!(r#"<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

const LOREM: &str = "The quick brown fox jumps over the lazy dog while Pack my box with five dozen liquor jugs and sphinx of black quartz judge my vow, again and again until the line must wrap more than once on the page.";

fn cases() -> Vec<(&'static str, Vec<u8>)> {
    let styled = format!(
        r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold </w:t></w:r><w:r><w:rPr><w:i/><w:u w:val="single"/></w:rPr><w:t>italic under </w:t></w:r><w:r><w:rPr><w:strike/><w:color w:val="FF0000"/></w:rPr><w:t>struck</w:t></w:r><w:hyperlink r:id="rIdL"><w:r><w:t> link</w:t></w:r></w:hyperlink></w:p>"#
    );
    let aligned = format!(
        r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>Centered line</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Right line</w:t></w:r></w:p>{}"#,
        para(LOREM)
    );
    let tabs = r#"<w:p><w:r><w:t>Name</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Value</w:t></w:r></w:p><w:p><w:pPr><w:ind w:left="720" w:firstLine="360"/></w:pPr><w:r><w:t>Indented paragraph text that wraps around when it is long enough to need it, which this one is.</w:t></w:r></w:p>"#.to_string();
    let table = r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b1</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>a2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_string();
    let many: String = (0..80)
        .map(|i| para(&format!("Paragraph {i}: {LOREM}")))
        .collect();
    let columns = format!(
        r#"<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/><w:cols w:num="2" w:space="720"/></w:sectPr></w:pPr><w:r><w:t>x</w:t></w:r></w:p>{}"#,
        para(LOREM)
    );
    let to_docx = |b: &str| docx(b);
    let mut out = vec![
        ("plain", to_docx(&para(LOREM))),
        ("styled", to_docx(&styled)),
        ("aligned", to_docx(&aligned)),
        ("tabs_indent", to_docx(&tabs)),
        ("table", to_docx(&table)),
        ("many_pages", to_docx(&many)),
        ("columns", to_docx(&columns)),
        (
            "nonlatin",
            to_docx(&para(
                "Привет мир \u{4e2d}\u{6587} caf\u{e9} \u{2014} \u{201c}q\u{201d}",
            )),
        ),
    ];
    for (name, file) in [
        ("sample", "sample.docx"),
        ("smarttag", "smarttag-pr.docx"),
        ("row_sdt", "row-content-controls.docx"),
    ] {
        let path = format!("{}/tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"));
        out.push((name, std::fs::read(path).unwrap()));
    }
    out
}

#[test]
fn courier_output_is_byte_stable() {
    let expected: &[(&str, u64)] = &[
        ("plain", 7284592837758742596),
        ("styled", 12854616644384491409),
        ("aligned", 18107219297071774245),
        ("tabs_indent", 16673373408050294724),
        ("table", 9992797185558723272),
        ("many_pages", 3399025174064952347),
        ("columns", 16478181600026551286),
        ("nonlatin", 16892130658081171166),
        ("sample", 6699086845947032117),
        ("smarttag", 2958886124268196559),
        ("row_sdt", 18007158070473349472),
    ];
    let mut got = Vec::new();
    for (name, bytes) in cases() {
        let pdf = pdf_of(&bytes);
        assert_eq!(pdf, pdf_of(&bytes), "{name} deterministic");
        got.push((name, fnv(&pdf)));
    }
    if expected.is_empty() {
        panic!("record: {got:#?}");
    }
    for (name, hash) in got {
        let want = expected.iter().find(|(n, _)| *n == name).unwrap().1;
        assert_eq!(hash, want, "{name}: the legacy PDF bytes moved");
    }
}
