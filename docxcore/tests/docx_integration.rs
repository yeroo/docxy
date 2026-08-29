//! Integration test: exercise the ZIP reader + DEFLATE decoder + XML parser
//! together against a real `.docx` fixture (DEFLATE-compressed parts).

use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::load;
use docxcore::model::{Block, Document, Inline, Table, TableRowBoundaryKind};
use docxcore::package::{load_package, save_package};
use docxcore::render::{RenderOptions, render};
use docxcore::xml::{Event, XmlParser};
use docxcore::zip::ZipArchive;

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.docx");
const ROW_CONTENT_CONTROLS: &[u8] = include_bytes!("fixtures/row-content-controls.docx");

const ROW_CONTROL_MARKERS: [&str; 5] = [
    "data-row-control=\"outer\"",
    "data-row-control=\"item-a\"",
    "data-row-control=\"item-b\"",
    "data-row-control=\"adjacent\"",
    "data-row-control=\"empty\"",
];

fn document_xml(data: &[u8]) -> String {
    let archive = ZipArchive::open(data).expect("fixture is a valid ZIP");
    let bytes = archive
        .read("word/document.xml")
        .expect("fixture has word/document.xml");
    String::from_utf8(bytes).expect("document.xml is UTF-8")
}

fn row_control_table(document: &Document) -> &Table {
    document
        .body
        .iter()
        .find_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .expect("fixture contains a table")
}

fn row_control_table_mut(document: &mut Document) -> &mut Table {
    document
        .body
        .iter_mut()
        .find_map(|block| match block {
            Block::Table(table) => Some(table),
            _ => None,
        })
        .expect("fixture contains a table")
}

fn assert_row_control_order(xml: &str) {
    let mut offset = 0;
    for marker in ROW_CONTROL_MARKERS {
        let found = xml[offset..]
            .find(marker)
            .unwrap_or_else(|| panic!("missing row-control marker {marker:?}"));
        offset += found + marker.len();
    }
}

fn sdt_wrapper_counts(xml: &str) -> (usize, usize) {
    let mut parser = XmlParser::new(xml);
    let mut opens = 0;
    let mut closes = 0;
    loop {
        match parser.next() {
            Event::Start if parser.name() == "w:sdt" => opens += 1,
            Event::End if parser.name() == "w:sdt" => closes += 1,
            Event::Eof => return (opens, closes),
            _ => {}
        }
    }
}

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

#[test]
fn row_content_control_package_roundtrip_preserves_wrappers_and_text() {
    let source_xml = document_xml(ROW_CONTENT_CONTROLS);
    assert_eq!(source_xml.matches("data-row-control=").count(), 5);
    assert_eq!(sdt_wrapper_counts(&source_xml), (7, 7));
    assert_row_control_order(&source_xml);

    let package = load_package(ROW_CONTENT_CONTROLS).expect("load row-control fixture");
    let source_parts = {
        let mut names = package.part_names();
        names.sort_unstable();
        names
    };
    assert_eq!(
        source_parts,
        vec![
            "[Content_Types].xml",
            "_rels/.rels",
            "word/_rels/document.xml.rels",
            "word/document.xml",
        ]
    );

    let table = row_control_table(&package.document);
    assert_eq!(table.rows.len(), 5, "controlled rows must remain visible");
    assert_eq!(table.grid, vec![2200, 2200, 2200]);
    assert_eq!(table.rows[1].cells[0].grid_span, 2);
    assert_eq!(table.rows[2].cells[1].grid_span, 2);
    assert!(
        table.rows[1].raw_props.iter().any(|raw| {
            raw.contains("<w:cantSplit/>")
                && raw.contains("<w:tblHeader/>")
                && raw.contains("<ux:rowFlag ux:val=\"row-kept\"/>")
        }),
        "row properties or extension XML were dropped"
    );
    assert_eq!(
        table.row_control_owners(),
        Ok(vec![vec![], vec![0, 1], vec![0, 4], vec![7], vec![]])
    );

    let (opens, closes, raw) = table.row_boundaries.iter().fold(
        (0, 0, 0),
        |(opens, closes, raw), boundary| match &boundary.kind {
            TableRowBoundaryKind::SdtOpen(_) => (opens + 1, closes, raw),
            TableRowBoundaryKind::SdtClose(_) => (opens, closes + 1, raw),
            TableRowBoundaryKind::Raw(xml) => {
                assert!(xml.contains("ux:between"), "unexpected raw child: {xml}");
                (opens, closes, raw + 1)
            }
        },
    );
    assert_eq!((opens, closes, raw), (5, 5, 1));

    let original_text = package.document.plain_text();
    for visible in [
        "Block sentinel",
        "Inline: inline sentinel!",
        "Plain row",
        "Order A merged",
        "Order B",
        "Adjacent row",
        "Tail row",
    ] {
        assert!(
            original_text.contains(visible),
            "missing visible fixture text {visible:?}: {original_text:?}"
        );
    }

    let saved = save_package(&package);
    let saved_xml = document_xml(&saved);
    assert_eq!(saved_xml.matches("data-row-control=").count(), 5);
    assert_eq!(sdt_wrapper_counts(&saved_xml), (7, 7));
    assert_row_control_order(&saved_xml);
    for preserved in [
        "<w15:repeatingSection w15:sectionTitle=\"Order\"/>",
        "<w15:repeatingSectionItem/>",
        "<ux:unknownProperty ux:val=\"outer-kept\"/>",
        "<ux:between ux:val=\"between-kept\"/>",
        "<ux:itemTail ux:val=\"a-tail\"/>",
        "<ux:itemTail ux:val=\"b-tail\"/>",
        "<ux:outerTail ux:val=\"outer-tail\"/>",
    ] {
        assert!(
            saved_xml.contains(preserved),
            "package round-trip dropped {preserved}"
        );
    }

    let reloaded = load_package(&saved).expect("reload saved row-control fixture");
    assert_eq!(reloaded.document, package.document);
    assert_eq!(reloaded.document.plain_text(), original_text);
    let mut saved_parts = reloaded.part_names();
    saved_parts.sort_unstable();
    assert_eq!(saved_parts, source_parts);
}

#[test]
fn targeted_cell_edit_remains_inside_its_row_control() {
    let mut package = load_package(ROW_CONTENT_CONTROLS).expect("load row-control fixture");
    let original_boundaries = row_control_table(&package.document).row_boundaries.clone();
    let table = row_control_table_mut(&mut package.document);
    let Block::Paragraph(paragraph) = &mut table.rows[2].cells[0].blocks[0] else {
        panic!("target cell starts with a paragraph");
    };
    let Inline::Run(run) = &mut paragraph.content[0] else {
        panic!("target paragraph starts with a run");
    };
    run.text = "Order B edited ✓".to_string();
    assert_eq!(table.row_boundaries, original_boundaries);
    assert_eq!(table.row_control_owners().unwrap()[2], vec![0, 4]);

    let saved = save_package(&package);
    let saved_xml = document_xml(&saved);
    let item_open = saved_xml
        .find("data-row-control=\"item-b\"")
        .expect("item-b opening wrapper");
    let edited = saved_xml
        .find("Order B edited ✓")
        .expect("edited cell payload");
    let item_tail = saved_xml
        .find("<ux:itemTail ux:val=\"b-tail\"/>")
        .expect("item-b closing metadata");
    assert!(
        item_open < edited && edited < item_tail,
        "edited payload moved outside item-b: {item_open}, {edited}, {item_tail}"
    );
    assert!(!saved_xml.contains(">Order B</w:t>"));

    let reloaded = load_package(&saved).expect("reload edited fixture");
    let table = row_control_table(&reloaded.document);
    assert_eq!(table.row_boundaries, original_boundaries);
    assert_eq!(table.row_control_owners().unwrap()[2], vec![0, 4]);
    assert_eq!(
        table.rows[2].cells[0].blocks[0].plain_text(),
        "Order B edited ✓"
    );
}

#[test]
fn block_and_inline_control_boundaries_stay_invisible_and_roundtrip() {
    let package = load_package(ROW_CONTENT_CONTROLS).expect("load row-control fixture");
    let block_open = package
        .document
        .body
        .iter()
        .position(|block| matches!(block, Block::Raw(raw) if raw.contains("BlockControl")))
        .expect("block control opening boundary");
    assert_eq!(
        package.document.body[block_open + 1].plain_text(),
        "Block sentinel"
    );
    assert!(matches!(
        package.document.body[block_open + 2],
        Block::Raw(_)
    ));

    let inline = package
        .document
        .body
        .iter()
        .find_map(|block| match block {
            Block::Paragraph(paragraph) if paragraph.plain_text() == "Inline: inline sentinel!" => {
                Some(paragraph)
            }
            _ => None,
        })
        .expect("inline control paragraph");
    assert_eq!(
        inline
            .content
            .iter()
            .filter(|item| matches!(item, Inline::Raw(_)))
            .count(),
        2,
        "inline wrapper should remain two invisible raw boundaries"
    );

    let rendered = render(
        &package.document,
        &RenderOptions {
            width: 200,
            ..RenderOptions::default()
        },
    )
    .iter()
    .map(|line| line.plain())
    .collect::<Vec<_>>()
    .join("\n");
    assert!(rendered.contains("Block sentinel"));
    assert!(rendered.contains("Inline: inline sentinel!"));
    for invisible in ["<w:sdt", "BlockControl", "InlineControl", "block-kept"] {
        assert!(
            !rendered.contains(invisible),
            "raw boundary metadata became visible: {invisible}"
        );
    }

    let saved = save_package(&package);
    let saved_xml = document_xml(&saved);
    for preserved in [
        "<w:alias w:val=\"BlockControl\"/>",
        "<ux:blockProperty xmlns:ux=\"urn:docxy:row-controls\" ux:val=\"block-kept\"/>",
        "<w:alias w:val=\"InlineControl\"/>",
        "<ux:inlineProperty xmlns:ux=\"urn:docxy:row-controls\" ux:val=\"inline-kept\"/>",
    ] {
        assert!(
            saved_xml.contains(preserved),
            "existing content-control boundary regressed: {preserved}"
        );
    }
    let reloaded = load_package(&saved).expect("reload block/inline controls");
    assert_eq!(reloaded.document, package.document);
}
