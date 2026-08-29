//! Full-package tracked-change interoperability coverage.
//!
//! The source fixture intentionally mixes every supported review scope with
//! comments, content controls, producer extensions, and two unsupported range
//! records. Each produced artifact is checked by both the independent OPC ZIP
//! reader and the docxcore package loader before its semantic assertions run.

use docxcore::editor::Editor;
use docxcore::load::load;
use docxcore::model::{Document, RevisionCategory, RevisionTarget, UnsupportedRevisionKind};
use docxcore::package::{Package, load_package, save_package, save_package_preserving_document};
use docxcore::review::RevisionOutcome;
use docxcore::xml::{Event, XmlParser};
use docxcore::zip::ZipArchive;
use docxcore::zipwrite::write_zip;

const DOCUMENT_XML: &str = include_str!("fixtures/revision-package.xml");
const COMMENTS_XML: &str = include_str!("fixtures/revision-comments.xml");

const SOURCE_TEXT: &str =
    "Start new nested old controlled removed End\nCell controlled cell new\nTail\n";
const ACCEPT_CURRENT_TEXT: &str =
    "Start new nested old controlled End\nCell controlled cell new\nTail\n";
const REJECT_CURRENT_TEXT: &str = "Start removed End\nCell controlled cell new\nTail\n";
const ACCEPT_ALL_TEXT: &str = "Start new controlled End\nCell controlled cell new\nTail\n";
const REJECT_ALL_TEXT: &str = "Start removed End\nCell controlled \nTail\n";

// These are deliberate exclusions, not fixture omissions. All review actions
// must report and retain them byte-for-byte.
const DELIBERATE_UNSUPPORTED_IDS: [&str; 2] = ["199", "198"];

fn fixture_docx() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/></Relationships>"#;
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="CurrentParagraph"><w:name w:val="Current Paragraph"/></w:style><w:style w:type="paragraph" w:styleId="PreviousParagraph"><w:name w:val="Previous Paragraph"/></w:style></w:styles>"#;

    write_zip(&[
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), root_rels.as_bytes().to_vec()),
        (
            "word/document.xml".to_string(),
            DOCUMENT_XML.as_bytes().to_vec(),
        ),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels.as_bytes().to_vec(),
        ),
        ("word/styles.xml".to_string(), styles.as_bytes().to_vec()),
        (
            "word/comments.xml".to_string(),
            COMMENTS_XML.as_bytes().to_vec(),
        ),
    ])
}

fn part_text(archive: &ZipArchive<'_>, name: &str) -> String {
    String::from_utf8(
        archive
            .read(name)
            .unwrap_or_else(|| panic!("package is missing {name}")),
    )
    .unwrap_or_else(|_| panic!("{name} is not UTF-8"))
}

fn document_xml(data: &[u8]) -> String {
    let archive = ZipArchive::open(data).expect("artifact is a ZIP package");
    part_text(&archive, "word/document.xml")
}

fn assert_well_formed_part(name: &str, xml: &str) {
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Text if parser.text().trim().is_empty() => {}
            Event::Start => break,
            event => panic!("{name} has no root element: {event:?}"),
        }
    }
    assert!(
        parser.skip_element_complete(),
        "{name} has unbalanced or mismatched XML"
    );
    loop {
        match parser.next() {
            Event::Text if parser.text().trim().is_empty() => {}
            Event::Eof => break,
            event => panic!("{name} has content after its root element: {event:?}"),
        }
    }
}

fn validate_artifact(data: &[u8]) {
    let archive = ZipArchive::open(data).expect("independent OPC reader opens artifact");
    for required in [
        "[Content_Types].xml",
        "_rels/.rels",
        "word/document.xml",
        "word/_rels/document.xml.rels",
        "word/styles.xml",
        "word/comments.xml",
    ] {
        assert!(
            archive.find(required).is_some(),
            "missing OPC part {required}"
        );
    }

    for entry in archive.entries() {
        if entry.name.ends_with(".xml") || entry.name.ends_with(".rels") {
            let xml = part_text(&archive, &entry.name);
            assert_well_formed_part(&entry.name, &xml);
        }
    }

    let content_types = part_text(&archive, "[Content_Types].xml");
    assert!(content_types.contains("PartName=\"/word/document.xml\""));
    assert!(content_types.contains("PartName=\"/word/comments.xml\""));
    let relationships = part_text(&archive, "word/_rels/document.xml.rels");
    assert!(relationships.contains("relationships/comments"));
    assert!(relationships.contains("Target=\"comments.xml\""));

    let xml = part_text(&archive, "word/document.xml");
    assert!(xml.contains("xmlns:ux=\"urn:docxy:revision-fixture\""));
    load(data).expect("plain DOCX loader accepts artifact");
    load_package(data).expect("package loader accepts artifact");
}

fn fixture_package() -> Package {
    let bytes = fixture_docx();
    validate_artifact(&bytes);
    load_package(&bytes).expect("load tracked-change fixture")
}

fn target_with_id(editor: &Editor, id: &str) -> RevisionTarget {
    editor
        .revision_locations()
        .into_iter()
        .find(|location| location.address.metadata.id.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("revision id {id}"))
        .address
        .target
}

fn revision_ids(document: &Document) -> Vec<String> {
    document
        .revisions()
        .iter()
        .map(|revision| {
            revision
                .metadata
                .id
                .as_deref()
                .expect("fixture revision has an id")
                .to_string()
        })
        .collect()
}

fn assert_only_deliberate_unsupported(document: &Document) {
    let revisions = document.revisions();
    assert_eq!(
        revision_ids(document),
        DELIBERATE_UNSUPPORTED_IDS.map(str::to_string)
    );
    assert!(matches!(
        revisions[0].category,
        RevisionCategory::Unsupported(UnsupportedRevisionKind::MoveFromRangeStart)
    ));
    assert!(matches!(
        revisions[1].category,
        RevisionCategory::Unsupported(UnsupportedRevisionKind::CustomXmlInsRangeStart)
    ));
}

fn assert_all_outcomes(outcomes: &[RevisionOutcome]) {
    assert_eq!(outcomes.len(), 12);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.is_applied())
            .count(),
        10
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RevisionOutcome::Unsupported { .. }))
            .count(),
        2
    );
}

fn save_editor_artifact(mut package: Package, editor: &Editor, name: &str) -> (Vec<u8>, Package) {
    package.document = editor.doc.clone();
    let saved = save_package(&package);
    validate_artifact(&saved);
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, &saved).expect("write review artifact");
    eprintln!("validated {}", path.display());
    let reloaded = load_package(&saved).expect("reload validated review artifact");
    (saved, reloaded)
}

#[test]
fn untouched_fixture_roundtrips_with_metadata_comments_and_controls() {
    let package = fixture_package();
    assert_eq!(package.document.plain_text(), SOURCE_TEXT);
    assert_eq!(
        revision_ids(&package.document),
        [
            "106", "105", "100", "101", "102", "103", "199", "198", "110", "111", "112", "113",
        ]
    );

    let insertion = package
        .document
        .revisions()
        .into_iter()
        .find(|revision| revision.metadata.id.as_deref() == Some("100"))
        .expect("outer insertion");
    assert_eq!(
        insertion.metadata.unknown_attributes,
        [("ux:session".to_string(), "outer-insert".to_string())]
    );
    assert_eq!(
        package.part("word/comments.xml"),
        Some(COMMENTS_XML.as_bytes())
    );

    let byte_preserved = save_package_preserving_document(&package);
    validate_artifact(&byte_preserved);
    assert_eq!(document_xml(&byte_preserved), DOCUMENT_XML);

    let saved = save_package(&package);
    validate_artifact(&saved);
    let xml = document_xml(&saved);
    for preserved in [
        "ux:session=\"outer-insert\"",
        "ux:audit ux:ticket=\"DOCX-101\"",
        "ux:controlData ux:val=\"kept\"",
        "ux:controlData ux:val=\"cell-kept\"",
        "w:commentRangeStart w:id=\"0\"",
        "w:commentRangeEnd w:id=\"0\"",
        "w:commentReference w:id=\"0\"",
        "ux:reason=\"unsupported-move\"",
        "ux:reason=\"unsupported-custom\"",
    ] {
        assert!(xml.contains(preserved), "normal save dropped {preserved}");
    }
    let reloaded = load_package(&saved).expect("reload untouched semantic save");
    assert_eq!(reloaded.document, package.document);
    assert_eq!(reloaded.document.plain_text(), SOURCE_TEXT);
    assert_eq!(
        reloaded.part("word/comments.xml"),
        Some(COMMENTS_XML.as_bytes())
    );
}

#[test]
fn accept_current_and_reject_current_save_and_reload_expected_text() {
    let package = fixture_package();
    let mut accepted = Editor::new(package.document.clone());
    let deletion = target_with_id(&accepted, "103");
    accepted.select_revision(deletion).expect("select deletion");
    assert!(matches!(
        accepted.accept_current_revision(),
        Some(RevisionOutcome::Applied { target, .. }) if target == deletion
    ));
    assert_eq!(accepted.doc.plain_text(), ACCEPT_CURRENT_TEXT);
    let (saved, reloaded) =
        save_editor_artifact(package, &accepted, "revision-accept-current.docx");
    let xml = document_xml(&saved);
    assert!(!xml.contains("<w:del w:id=\"103\""));
    assert!(!xml.contains(">removed </w:delText>"));
    assert!(xml.contains("<w:ins w:id=\"100\""));
    assert_eq!(reloaded.document.plain_text(), ACCEPT_CURRENT_TEXT);

    let package = fixture_package();
    let mut rejected = Editor::new(package.document.clone());
    let insertion = target_with_id(&rejected, "100");
    rejected
        .select_revision(insertion)
        .expect("select insertion");
    assert!(matches!(
        rejected.reject_current_revision(),
        Some(RevisionOutcome::Applied { target, .. }) if target == insertion
    ));
    assert_eq!(rejected.doc.plain_text(), REJECT_CURRENT_TEXT);
    let (saved, reloaded) =
        save_editor_artifact(package, &rejected, "revision-reject-current.docx");
    let xml = document_xml(&saved);
    for removed_id in ["100", "101", "102"] {
        assert!(
            !xml.contains(&format!("w:id=\"{removed_id}\"")),
            "rejecting the outer insertion retained nested revision {removed_id}"
        );
    }
    assert!(xml.contains("<w:del w:id=\"103\""));
    assert_eq!(reloaded.document.plain_text(), REJECT_CURRENT_TEXT);
}

#[test]
fn accept_all_is_undoable_and_roundtrips_current_properties() {
    let package = fixture_package();
    let mut editor = Editor::new(package.document.clone());
    let original = editor.doc.clone();
    let outcomes = editor.accept_all_revisions();
    assert_all_outcomes(&outcomes);
    assert_eq!(editor.doc.plain_text(), ACCEPT_ALL_TEXT);
    assert_only_deliberate_unsupported(&editor.doc);
    let accepted = editor.doc.clone();

    assert!(editor.undo());
    assert_eq!(editor.doc, original);
    assert!(!editor.undo(), "accept-all creates one history entry");
    assert!(editor.redo());
    assert_eq!(editor.doc, accepted);
    assert!(!editor.redo());

    let (saved, reloaded) = save_editor_artifact(package, &editor, "revision-accept-all.docx");
    let xml = document_xml(&saved);
    assert!(!xml.contains("PrChange"));
    assert!(!xml.contains("<w:ins "));
    assert!(!xml.contains("<w:del "));
    for current in [
        "CurrentParagraph",
        "CurrentTable",
        "w:fill=\"ABCDEF\"",
        "w:fill=\"00FF00\"",
        "<w:tblHeader/>",
        "w:orient=\"landscape\"",
        "<w:b/>",
    ] {
        assert!(xml.contains(current), "accept-all lost {current}");
    }
    for removed_prior in [
        "PreviousParagraph",
        "PreviousTable",
        "w:fill=\"FFFF00\"",
        "DOCX-101",
    ] {
        assert!(
            !xml.contains(removed_prior),
            "accept-all retained prior property data {removed_prior}"
        );
    }
    assert!(xml.contains("ux:reason=\"unsupported-move\""));
    assert!(xml.contains("ux:reason=\"unsupported-custom\""));
    assert!(xml.contains("w:commentReference w:id=\"0\""));
    assert!(xml.contains("ux:controlData ux:val=\"kept\""));
    assert_eq!(reloaded.document.plain_text(), ACCEPT_ALL_TEXT);
    assert_only_deliberate_unsupported(&reloaded.document);
}

#[test]
fn reject_all_is_undoable_and_roundtrips_prior_properties() {
    let package = fixture_package();
    let mut editor = Editor::new(package.document.clone());
    let original = editor.doc.clone();
    let outcomes = editor.reject_all_revisions();
    assert_all_outcomes(&outcomes);
    assert_eq!(editor.doc.plain_text(), REJECT_ALL_TEXT);
    assert_only_deliberate_unsupported(&editor.doc);
    let rejected = editor.doc.clone();

    assert!(editor.undo());
    assert_eq!(editor.doc, original);
    assert!(!editor.undo(), "reject-all creates one history entry");
    assert!(editor.redo());
    assert_eq!(editor.doc, rejected);
    assert!(!editor.redo());

    let (saved, reloaded) = save_editor_artifact(package, &editor, "revision-reject-all.docx");
    let xml = document_xml(&saved);
    assert!(!xml.contains("PrChange"));
    assert!(!xml.contains("<w:ins "));
    assert!(!xml.contains("<w:del "));
    assert!(xml.contains("<w:t xml:space=\"preserve\">removed </w:t>"));
    for prior in [
        "PreviousParagraph",
        "PreviousTable",
        "w:fill=\"FFFF00\"",
        "<w:cantSplit/>",
        "w:w=\"11906\"",
    ] {
        assert!(xml.contains(prior), "reject-all lost {prior}");
    }
    for removed_current in [
        "CurrentParagraph",
        "CurrentTable",
        "w:fill=\"ABCDEF\"",
        "w:fill=\"00FF00\"",
        "<w:tblHeader/>",
        "w:orient=\"landscape\"",
    ] {
        assert!(
            !xml.contains(removed_current),
            "reject-all retained current property data {removed_current}"
        );
    }
    assert!(xml.contains("ux:reason=\"unsupported-move\""));
    assert!(xml.contains("ux:reason=\"unsupported-custom\""));
    assert!(xml.contains("w:commentReference w:id=\"0\""));
    assert!(xml.contains("ux:controlData ux:val=\"cell-kept\""));
    assert_eq!(reloaded.document.plain_text(), REJECT_ALL_TEXT);
    assert_only_deliberate_unsupported(&reloaded.document);
}
