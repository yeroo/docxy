use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{
    Block, Inline, PropertyScope, RevisionCategory, RevisionKind, RevisionTarget,
};
use docxcore::review::{MalformedRevisionReason, RevisionOutcome};
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const PROPERTY_FIXTURE: &str = include_str!("fixtures/property-changes.xml");

fn parse(xml: &str) -> docxcore::model::Document {
    parse_document_xml(xml, &Relationships::default())
}

fn target_with_id(document: &docxcore::model::Document, id: &str) -> RevisionTarget {
    document
        .revisions()
        .into_iter()
        .find(|revision| revision.metadata.id.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("revision id {id}"))
        .target
}

#[test]
fn empty_adjacent_hyperlink_field_and_content_control_transforms_are_valid() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body><w:p>",
            "<w:ins w:id=\"1\"/>",
            "<w:ins w:id=\"2\"><w:hyperlink r:id=\"rId7\" w:anchor=\"bookmark\"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:ins>",
            "<w:bookmarkStart w:id=\"4\" w:name=\"edge\"/>",
            "<w:del w:id=\"3\"><w:fldSimple w:instr=\" DATE \"><w:r><w:delText>old date</w:delText></w:r></w:fldSimple>",
            "<w:sdt><w:sdtPr><w:tag w:val=\"kept\"/></w:sdtPr><w:sdtContent><w:r><w:delText>inside</w:delText></w:r></w:sdtContent></w:sdt>",
            "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:delInstrText> PAGE </w:delInstrText></w:r>",
            "<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:delText>7</w:delText></w:r>",
            "<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>",
            "</w:del><w:bookmarkEnd w:id=\"4\"/>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    assert!(
        document
            .accept_revision(target_with_id(&document, "1"))
            .is_applied()
    );
    assert!(
        document
            .accept_revision(target_with_id(&document, "2"))
            .is_applied()
    );
    assert!(
        document
            .reject_revision(target_with_id(&document, "3"))
            .is_applied()
    );

    assert_eq!(document.plain_text(), "linkold dateinside7\n");
    let saved = document_to_xml(&document);
    assert!(!saved.contains("<w:ins "));
    assert!(!saved.contains("<w:ins>"));
    assert!(!saved.contains("<w:ins/"));
    assert!(!saved.contains("<w:del "));
    assert!(!saved.contains("delText"));
    assert!(saved.contains("<w:hyperlink r:id=\"rId7\""));
    assert!(saved.contains("<w:fldSimple w:instr=\" DATE \""));
    assert!(saved.contains("<w:instrText> PAGE </w:instrText>"));
    assert!(saved.contains("w:fldCharType=\"begin\""));
    assert!(saved.contains("w:fldCharType=\"end\""));
    assert!(saved.contains("<w:sdtPr><w:tag w:val=\"kept\""));
    assert!(saved.contains("bookmarkStart"));
    assert!(saved.contains("bookmarkEnd"));
    assert_eq!(parse(&saved).plain_text(), document.plain_text());
}

#[test]
fn reject_all_restores_every_property_scope_and_preserves_malformed_record() {
    let mut document = parse(PROPERTY_FIXTURE);
    let outcomes = document.reject_all_revisions();
    assert_eq!(outcomes.len(), 9);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.is_applied())
            .count(),
        8
    );
    assert!(matches!(
        outcomes
            .iter()
            .find(|outcome| matches!(outcome, RevisionOutcome::Malformed { .. }))
            .unwrap(),
        RevisionOutcome::Malformed {
            reason: MalformedRevisionReason::PropertySnapshot {
                scope: PropertyScope::Run
            },
            ..
        }
    ));

    let Block::Paragraph(paragraph) = &document.body[0] else {
        panic!("paragraph")
    };
    assert_eq!(
        paragraph.props.style_id.as_deref(),
        Some("PreviousParagraph")
    );
    assert!(paragraph.props.property_change.is_none());
    assert!(paragraph.props.section_property_change.is_none());
    assert!(
        paragraph
            .props
            .section_break
            .as_deref()
            .unwrap()
            .contains("w:w=\"11906\"")
    );
    let Inline::Run(run) = &paragraph.content[0] else {
        panic!("run")
    };
    assert!(run.props.bold);
    assert!(!run.props.italic);
    assert!(run.props.underline);
    assert_eq!(run.props.style_id.as_deref(), Some("PreviousCharacter"));

    let Block::Paragraph(edge_cases) = &document.body[1] else {
        panic!("edge cases")
    };
    let changes = edge_cases
        .content
        .iter()
        .map(|inline| match inline {
            Inline::Run(run) => run.props.property_change.is_some(),
            _ => panic!("run"),
        })
        .collect::<Vec<_>>();
    assert_eq!(changes, [false, true, false]);

    let Block::Table(table) = &document.body[2] else {
        panic!("table")
    };
    assert!(
        table
            .raw_tblpr
            .as_deref()
            .unwrap()
            .contains("PreviousTable")
    );
    assert!(table.rows[0].raw_props[0].contains("w:cantSplit"));
    assert_eq!(table.rows[0].cells[0].grid_span, 1);
    assert!(
        table.rows[0].cells[0]
            .raw_tcpr
            .as_deref()
            .unwrap()
            .contains("FFFF00")
    );

    let saved = document_to_xml(&document);
    assert_eq!(saved.matches("PrChange").count(), 2, "one start/end pair");
    assert!(saved.contains("<w:rPrChange w:id=\"18\""));
    assert!(!saved.contains("CurrentTable"));
    assert!(saved.contains("w:lang w:val=\"fr-FR\""));
    let reparsed = parse(&saved);
    assert_eq!(reparsed.revisions().len(), 1);
}

#[test]
fn accept_all_keeps_current_mixed_properties_without_needing_prior_snapshots() {
    let mut document = parse(PROPERTY_FIXTURE);
    let outcomes = document.accept_all_revisions();
    assert_eq!(outcomes.len(), 9);
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));

    let saved = document_to_xml(&document);
    assert!(saved.contains("CurrentParagraph"));
    assert!(saved.contains("CurrentCharacter"));
    assert!(saved.contains("CurrentTable"));
    assert!(saved.contains("w:fill=\"00FF00\""));
    assert!(!saved.contains("PreviousParagraph"));
    assert!(!saved.contains("PreviousTable"));
    assert!(parse(&saved).revisions().is_empty());
}

#[test]
fn property_action_inside_wrapper_preserves_wrapper_and_cue_provenance() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:ins w:id=\"40\" w:author=\"outer\"><w:r><w:rPr><w:i/>",
            "<w:rPrChange w:id=\"41\"><w:rPr><w:b/></w:rPr></w:rPrChange>",
            "</w:rPr><w:t>changed</w:t></w:r></w:ins>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    assert!(
        document
            .reject_revision(target_with_id(&document, "41"))
            .is_applied()
    );

    let saved_inside_wrapper = document_to_xml(&document);
    assert!(saved_inside_wrapper.contains("<w:ins w:id=\"40\" w:author=\"outer\""));
    assert!(saved_inside_wrapper.contains("<w:b/>"));
    assert!(!saved_inside_wrapper.contains("<w:i/>"));
    assert!(
        !saved_inside_wrapper.contains("<w:u"),
        "display cue leaked into XML"
    );
    assert!(!saved_inside_wrapper.contains("rPrChange"));

    let mut reloaded = parse(&saved_inside_wrapper);
    let outer = target_with_id(&reloaded, "40");
    let Block::Paragraph(paragraph) = &reloaded.body[0] else {
        panic!("paragraph")
    };
    let Inline::Revision { content, .. } = &paragraph.content[0] else {
        panic!("revision")
    };
    let Inline::Run(run) = &content[0] else {
        panic!("run")
    };
    assert!(run.props.bold);
    assert!(run.props.underline, "insertion cue missing after reload");

    assert!(reloaded.accept_revision(outer).is_applied());
    let Block::Paragraph(paragraph) = &reloaded.body[0] else {
        panic!("paragraph")
    };
    let Inline::Run(run) = &paragraph.content[0] else {
        panic!("run")
    };
    assert!(run.props.bold);
    assert!(!run.props.underline, "wrapper-only cue survived accept");
}

#[test]
fn changing_a_property_inside_deletion_rebuilds_legal_deleted_text() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:del w:id=\"45\"><w:r><w:rPr><w:i/>",
            "<w:rPrChange w:id=\"46\"><w:rPr><w:b/></w:rPr></w:rPrChange>",
            "</w:rPr><w:delText>deleted</w:delText></w:r></w:del>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    assert!(
        document
            .accept_revision(target_with_id(&document, "46"))
            .is_applied()
    );
    let saved = document_to_xml(&document);
    assert!(saved.contains("<w:del w:id=\"45\""));
    assert!(saved.contains("<w:delText xml:space=\"preserve\">deleted</w:delText>"));
    assert!(!saved.contains("<w:t xml:space=\"preserve\">deleted"));
    assert!(!saved.contains("rPrChange"));

    let mut reloaded = parse(&saved);
    assert!(
        reloaded
            .reject_revision(target_with_id(&reloaded, "45"))
            .is_applied()
    );
    assert_eq!(reloaded.plain_text(), "deleted\n");
    assert!(!document_to_xml(&reloaded).contains("delText"));
}

#[test]
fn row_property_rejection_preserves_unrelated_table_exceptions() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl>",
            "<w:tr><w:tblPrEx><w:tblW w:w=\"1234\" w:type=\"dxa\"/><w:vendorRow/></w:tblPrEx>",
            "<w:trPr><w:tblHeader/><w:trPrChange w:id=\"47\"><w:trPr><w:cantSplit/></w:trPr></w:trPrChange></w:trPr>",
            "<w:tc><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    assert!(
        document
            .reject_revision(target_with_id(&document, "47"))
            .is_applied()
    );
    let saved = document_to_xml(&document);
    assert!(saved.contains("<w:tblPrEx><w:tblW w:w=\"1234\""));
    assert!(saved.contains("<w:vendorRow/>"));
    assert!(saved.contains("<w:trPr><w:cantSplit/></w:trPr>"));
    assert!(!saved.contains("tblHeader"));
    assert!(!saved.contains("trPrChange"));
}

#[test]
fn outcomes_remain_in_document_order_for_nested_and_adjacent_revisions() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:ins w:id=\"50\"><w:del w:id=\"51\"><w:r><w:delText>x</w:delText></w:r></w:del></w:ins>",
            "<w:ins w:id=\"52\"><w:r><w:t>y</w:t></w:r></w:ins>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    let outcomes = document.accept_all_revisions();
    let categories = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            RevisionOutcome::Applied { category, .. } => category,
            other => panic!("unexpected outcome {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        categories,
        [
            RevisionCategory::Inline(RevisionKind::Insert),
            RevisionCategory::Inline(RevisionKind::Delete),
            RevisionCategory::Inline(RevisionKind::Insert),
        ]
    );
    assert_eq!(document.plain_text(), "y\n");
}
