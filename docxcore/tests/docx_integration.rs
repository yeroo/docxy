//! Integration test: exercise the ZIP reader + DEFLATE decoder + XML parser
//! together against a real `.docx` fixture (DEFLATE-compressed parts).

use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::load;
use docxcore::model::Block;
use docxcore::package::{load_package, save_package};
use docxcore::render::{RenderOptions, render};
use docxcore::xml::{Event, XmlParser};
use docxcore::zip::ZipArchive;

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.docx");

#[test]
fn opens_real_docx_and_lists_core_parts() {
    let arc = ZipArchive::open(SAMPLE).expect("sample.docx is a valid ZIP");
    // A .docx must contain these OPC parts.
    assert!(arc.find("[Content_Types].xml").is_some());
    assert!(arc.find("word/document.xml").is_some());
}

#[test]
fn extracts_and_parses_document_xml() {
    let arc = ZipArchive::open(SAMPLE).expect("open");
    let bytes = arc.read("word/document.xml").expect("extract document.xml");
    let xml = std::str::from_utf8(&bytes).expect("document.xml is utf-8");

    // The decompressed part should be well-formed WordprocessingML.
    assert!(xml.contains("<w:document"));
    assert!(xml.contains("<w:body"));

    // Pull the visible text out via the parser and confirm we got something.
    let mut p = XmlParser::new(xml);
    let mut in_text = false;
    let mut text = String::new();
    let mut paragraphs = 0usize;
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:t" => in_text = true,
                "w:p" => paragraphs += 1,
                _ => {}
            },
            Event::End => {
                if p.name() == "w:t" {
                    in_text = false;
                }
            }
            Event::Text => {
                if in_text {
                    XmlParser::append_decoded(p.text(), &mut text);
                }
            }
            Event::Eof => break,
        }
    }
    assert!(paragraphs >= 1, "expected at least one paragraph");
    assert!(
        !text.trim().is_empty(),
        "expected some visible text, got {text:?}"
    );
}

#[test]
fn extracting_missing_part_is_none() {
    let arc = ZipArchive::open(SAMPLE).expect("open");
    assert!(arc.read("word/nonexistent.xml").is_none());
}

#[test]
fn load_builds_document_model() {
    let doc = load(SAMPLE).expect("load model from sample.docx");
    assert!(!doc.body.is_empty(), "document should have blocks");
    let paragraphs = doc
        .body
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(_)))
        .count();
    assert!(paragraphs >= 1, "expected at least one paragraph block");
    assert!(
        !doc.plain_text().trim().is_empty(),
        "model should yield visible text: {:?}",
        doc.plain_text()
    );
}

#[test]
fn end_to_end_load_then_render() {
    let doc = load(SAMPLE).expect("load");
    let width = 72;
    let lines = render(
        &doc,
        &RenderOptions {
            width,
            ..RenderOptions::default()
        },
    );
    assert!(!lines.is_empty(), "render produced no lines");
    for l in &lines {
        assert!(
            l.width() <= width,
            "rendered line exceeds width: {:?}",
            l.plain()
        );
    }
    let text: String = lines
        .iter()
        .map(|l| l.plain())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !text.trim().is_empty(),
        "rendered output has no visible text"
    );
}

#[test]
fn end_to_end_load_then_pdf() {
    let doc = load(SAMPLE).expect("load");
    let pdf = to_pdf(&doc, &PdfOptions::default());
    assert!(pdf.starts_with(b"%PDF-1."), "missing PDF header");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Type /Catalog") && text.contains("/Contents "));
    assert!(text.trim_end().ends_with("%%EOF"), "missing PDF trailer");

    // Also write it out so the result can be opened/inspected by hand.
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("sample.pdf");
    std::fs::write(&path, &pdf).expect("write sample.pdf");
    eprintln!("wrote {}", path.display());
}

#[test]
fn real_docx_save_roundtrip_is_lossless_for_model() {
    let pkg1 = load_package(SAMPLE).expect("load_package");
    let original_parts = {
        let mut n = pkg1.part_names();
        n.sort();
        n.into_iter().map(str::to_string).collect::<Vec<_>>()
    };

    let saved = save_package(&pkg1);
    // The saved bytes are a valid ZIP that re-reads.
    let pkg2 = load_package(&saved).expect("reload saved package");

    // The modeled document is identical after save -> reload.
    assert_eq!(
        pkg1.document, pkg2.document,
        "model changed across save round-trip"
    );

    // Every original part is still present (nothing dropped from the container).
    let saved_parts = {
        let mut n = pkg2.part_names();
        n.sort();
        n.into_iter().map(str::to_string).collect::<Vec<_>>()
    };
    assert_eq!(
        original_parts, saved_parts,
        "a container part was lost on save"
    );

    // The saved file is still loadable by the plain `load` entry point too.
    let _doc = load(&saved).expect("plain load of saved file");

    // Persist for manual opening in Word.
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("sample-resaved.docx");
    std::fs::write(&path, &saved).expect("write resaved docx");
    eprintln!("wrote {}", path.display());
}
