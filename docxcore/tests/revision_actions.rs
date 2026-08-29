use docxcore::load::{Relationships, parse_document_xml, parse_rels_xml};
use docxcore::model::{
    Block, Inline, PropertyScope, RevisionCategory, RevisionKind, RevisionTarget,
    UnsupportedRevisionKind,
};
use docxcore::package::{load_package, new_package, save_package};
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

#[test]
fn trailing_section_change_is_actionable_and_round_trips_once() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p>",
            "<w:sectPr><w:pgSz w:w=\"15840\"/><w:sectPrChange w:id=\"70\">",
            "<w:sectPr><w:pgSz w:w=\"12240\"/></w:sectPr></w:sectPrChange></w:sectPr>",
            "</w:body></w:document>"
        ),
        W_NS = W_NS
    );

    let original = parse(&xml);
    assert!(matches!(
        original.revisions()[0].category,
        RevisionCategory::Property(PropertyScope::Section)
    ));

    let mut accepted = original.clone();
    let accepted_target = target_with_id(&accepted, "70");
    assert!(accepted.accept_revision(accepted_target).is_applied());
    let accepted_xml = document_to_xml(&accepted);
    assert!(accepted_xml.contains("w:w=\"15840\""));
    assert!(!accepted_xml.contains("sectPrChange"));
    assert_eq!(accepted_xml.matches("<w:sectPr").count(), 1);

    let mut rejected = original;
    let rejected_target = target_with_id(&rejected, "70");
    assert!(rejected.reject_revision(rejected_target).is_applied());
    let rejected_xml = document_to_xml(&rejected);
    assert!(rejected_xml.contains("w:w=\"12240\""));
    assert!(!rejected_xml.contains("w:w=\"15840\""));
    assert_eq!(rejected_xml.matches("<w:sectPr").count(), 1);

    let package = new_package(rejected);
    let reloaded = load_package(&save_package(&package)).expect("reload package");
    assert_eq!(
        document_to_xml(&reloaded.document)
            .matches("<w:sectPr")
            .count(),
        1
    );
    assert!(reloaded.document.revisions().is_empty());
}

#[test]
fn external_hyperlink_keeps_and_reviews_nested_revision_content() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body><w:p>",
            "<w:hyperlink r:id=\"rId7\" w:history=\"1\"><w:ins w:id=\"71\" w:author=\"Ada\">",
            "<w:r><w:t>linked</w:t></w:r></w:ins><w:fldSimple w:instr=\" PAGE \"/>",
            "</w:hyperlink></w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let rels = parse_rels_xml(
        "<Relationships><Relationship Id=\"rId7\" Target=\"https://example.test/\" TargetMode=\"External\"/></Relationships>",
    );
    let mut document = parse_document_xml(&xml, &rels);
    assert_eq!(document.plain_text(), "linked\n");
    let untouched = document_to_xml(&document);
    assert!(untouched.contains("w:history=\"1\""));
    assert!(untouched.contains("<w:ins w:id=\"71\""));
    assert!(untouched.contains("w:fldSimple"));

    let target = target_with_id(&document, "71");
    assert!(document.accept_revision(target).is_applied());
    let saved = document_to_xml(&document);
    assert!(saved.contains("<w:hyperlink r:id=\"rId7\" w:history=\"1\""));
    assert!(saved.contains("linked"));
    assert!(saved.contains("w:fldSimple"));
    assert!(!saved.contains("<w:ins"));
}

#[test]
fn reviewed_external_hyperlink_keeps_unknown_attributes_after_two_roundtrips() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body><w:p>",
            "<w:hyperlink r:id=\"rId7\" w:history=\"1\" w:tooltip=\"tip\" w:tgtFrame=\"_blank\">",
            "<w:ins w:id=\"710\"><w:r><w:t>linked</w:t></w:r></w:ins>",
            "</w:hyperlink></w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let rels = parse_rels_xml(
        "<Relationships><Relationship Id=\"rId7\" Target=\"https://example.test/\" TargetMode=\"External\"/></Relationships>",
    );
    let mut document = parse_document_xml(&xml, &rels);
    assert!(
        document
            .accept_revision(target_with_id(&document, "710"))
            .is_applied()
    );

    let once = document_to_xml(&document);
    let reloaded = parse_document_xml(&once, &rels);
    let twice = document_to_xml(&reloaded);
    assert!(twice.contains("w:history=\"1\""), "{twice}");
    assert!(twice.contains("w:tooltip=\"tip\""), "{twice}");
    assert!(twice.contains("w:tgtFrame=\"_blank\""), "{twice}");
    assert!(twice.contains("<w:hyperlink r:id=\"rId7\""), "{twice}");
    assert!(twice.contains("linked"), "{twice}");
}

#[test]
fn preserved_self_closing_hyperlink_rebuilds_as_balanced_xml() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body><w:p>",
            "<w:hyperlink r:id=\"rId7\" w:history=\"1\"/>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let rels = parse_rels_xml(
        "<Relationships><Relationship Id=\"rId7\" Target=\"https://example.test/\" TargetMode=\"External\"/></Relationships>",
    );

    let once = document_to_xml(&parse_document_xml(&xml, &rels));
    assert!(
        once.contains("<w:hyperlink r:id=\"rId7\" w:history=\"1\"></w:hyperlink>"),
        "{once}"
    );
    assert!(!once.contains("/></w:hyperlink>"), "{once}");

    let twice = document_to_xml(&parse_document_xml(&once, &rels));
    assert!(twice.contains("w:history=\"1\""), "{twice}");
    assert!(!twice.contains("/></w:hyperlink>"), "{twice}");
}

#[test]
fn split_toc_hyperlink_preserves_opener_attributes_on_each_segment() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:hyperlink w:anchor=\"_Toc1\" w:history=\"1\" w:tooltip=\"toc\">",
            "<w:r><w:t>Intro</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>9</w:t></w:r>",
            "</w:hyperlink></w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );

    let document = parse(&xml);
    assert_eq!(document.plain_text(), "Intro\t9\n");
    let once = document_to_xml(&document);
    assert_eq!(once.matches("w:history=\"1\"").count(), 2, "{once}");
    assert_eq!(once.matches("w:tooltip=\"toc\"").count(), 2, "{once}");
    assert!(once.contains("<w:tab/>"), "{once}");

    let twice = document_to_xml(&parse(&once));
    assert_eq!(twice.matches("w:history=\"1\"").count(), 2, "{twice}");
    assert_eq!(twice.matches("w:tooltip=\"toc\"").count(), 2, "{twice}");
    assert!(twice.contains("<w:tab/>"), "{twice}");
}

#[test]
fn internal_anchor_hyperlink_keeps_nested_revision_inside_the_link() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:hyperlink w:anchor=\"target\" w:history=\"1\"><w:ins w:id=\"711\">",
            "<w:r><w:t>jump</w:t></w:r></w:ins></w:hyperlink>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    let untouched = document_to_xml(&document);
    assert!(untouched.contains("<w:hyperlink w:anchor=\"target\""));
    assert!(untouched.contains("<w:ins w:id=\"711\""));

    assert!(
        document
            .accept_revision(target_with_id(&document, "711"))
            .is_applied()
    );
    let accepted = document_to_xml(&document);
    assert!(accepted.contains("<w:hyperlink w:anchor=\"target\""));
    assert!(accepted.contains("w:history=\"1\""));
    assert!(accepted.contains("jump"));
    assert!(!accepted.contains("<w:ins"));

    let twice = document_to_xml(&parse(&accepted));
    assert!(twice.contains("<w:hyperlink w:anchor=\"target\""));
    assert!(twice.contains("w:history=\"1\""));
    assert!(twice.contains("jump"));
}

#[test]
fn destructive_outer_action_refuses_to_drop_nested_unsupported_revision() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:ins w:id=\"72\"><w:moveFromRangeStart w:id=\"73\"/>",
            "<w:r><w:t>kept</w:t></w:r></w:ins>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    let before = document_to_xml(&document);
    let outcome = document.reject_revision(target_with_id(&document, "72"));
    assert!(matches!(
        outcome,
        RevisionOutcome::Unsupported {
            kind: UnsupportedRevisionKind::MoveFromRangeStart,
            ..
        }
    ));
    assert_eq!(document_to_xml(&document), before);
}

#[test]
fn cell_revisions_are_enumerated_as_unsupported_and_preserved() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl><w:tr><w:tc><w:tcPr>",
            "<w:cellIns w:id=\"74\"/><w:cellDel w:id=\"75\"/><w:cellMerge w:id=\"76\"/>",
            "</w:tcPr><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    let kinds = document
        .revisions()
        .iter()
        .map(|address| address.category.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            RevisionCategory::Unsupported(UnsupportedRevisionKind::CellInsert),
            RevisionCategory::Unsupported(UnsupportedRevisionKind::CellDelete),
            RevisionCategory::Unsupported(UnsupportedRevisionKind::CellMerge),
        ]
    );
    let outcome = document.accept_revision(target_with_id(&document, "75"));
    assert!(matches!(
        outcome,
        RevisionOutcome::Unsupported {
            kind: UnsupportedRevisionKind::CellDelete,
            ..
        }
    ));
    let saved = document_to_xml(&document);
    assert!(saved.contains("cellIns") && saved.contains("cellDel") && saved.contains("cellMerge"));
}

#[test]
fn rejecting_cell_properties_preserves_sibling_cell_revision_records() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl><w:tr><w:tc><w:tcPr>",
            "<w:shd w:fill=\"CURRENT\"/><w:cellIns w:id=\"740\"/>",
            "<w:tcPrChange w:id=\"741\"><w:tcPr><w:shd w:fill=\"PRIOR\"/></w:tcPr></w:tcPrChange>",
            "</w:tcPr><w:p/></w:tc></w:tr></w:tbl></w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    assert!(
        document
            .reject_revision(target_with_id(&document, "741"))
            .is_applied()
    );
    let saved = document_to_xml(&document);
    assert!(saved.contains("w:fill=\"PRIOR\""), "{saved}");
    assert!(!saved.contains("w:fill=\"CURRENT\""), "{saved}");
    assert!(saved.contains("<w:cellIns w:id=\"740\""), "{saved}");
    assert!(!saved.contains("tcPrChange"), "{saved}");

    let reparsed = parse(&saved);
    let remaining = reparsed.revisions();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].metadata.id.as_deref(), Some("740"));
    assert!(matches!(
        remaining[0].category,
        RevisionCategory::Unsupported(UnsupportedRevisionKind::CellInsert)
    ));
}

#[test]
fn every_inline_unsupported_revision_tag_maps_to_its_public_kind() {
    let cases = [
        ("moveFrom", UnsupportedRevisionKind::MoveFrom),
        ("moveTo", UnsupportedRevisionKind::MoveTo),
        (
            "moveFromRangeStart",
            UnsupportedRevisionKind::MoveFromRangeStart,
        ),
        (
            "moveFromRangeEnd",
            UnsupportedRevisionKind::MoveFromRangeEnd,
        ),
        (
            "moveToRangeStart",
            UnsupportedRevisionKind::MoveToRangeStart,
        ),
        ("moveToRangeEnd", UnsupportedRevisionKind::MoveToRangeEnd),
        (
            "customXmlInsRangeStart",
            UnsupportedRevisionKind::CustomXmlInsRangeStart,
        ),
        (
            "customXmlInsRangeEnd",
            UnsupportedRevisionKind::CustomXmlInsRangeEnd,
        ),
        (
            "customXmlDelRangeStart",
            UnsupportedRevisionKind::CustomXmlDelRangeStart,
        ),
        (
            "customXmlDelRangeEnd",
            UnsupportedRevisionKind::CustomXmlDelRangeEnd,
        ),
        (
            "customXmlMoveFromRangeStart",
            UnsupportedRevisionKind::CustomXmlMoveFromRangeStart,
        ),
        (
            "customXmlMoveFromRangeEnd",
            UnsupportedRevisionKind::CustomXmlMoveFromRangeEnd,
        ),
        (
            "customXmlMoveToRangeStart",
            UnsupportedRevisionKind::CustomXmlMoveToRangeStart,
        ),
        (
            "customXmlMoveToRangeEnd",
            UnsupportedRevisionKind::CustomXmlMoveToRangeEnd,
        ),
        ("conflictIns", UnsupportedRevisionKind::ConflictInsert),
        ("conflictDel", UnsupportedRevisionKind::ConflictDelete),
    ];

    for (index, (tag, expected)) in cases.into_iter().enumerate() {
        let xml = format!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p><w:{tag} w:id=\"{index}\"/></w:p></w:body></w:document>"
        );
        let document = parse(&xml);
        assert_eq!(
            document.revisions()[0].category,
            RevisionCategory::Unsupported(expected),
            "{tag}"
        );
    }
}

#[test]
fn rejecting_absent_property_snapshots_clears_every_container_scope() {
    let xml = format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body>",
            "<w:p><w:pPr><w:pStyle w:val=\"Now\"/><w:pPrChange w:id=\"80\"/></w:pPr><w:r><w:t>p</w:t></w:r></w:p>",
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"Now\"/><w:tblPrChange w:id=\"81\"/></w:tblPr>",
            "<w:tr><w:trPr><w:tblHeader/><w:trPrChange w:id=\"82\"/></w:trPr>",
            "<w:tc><w:tcPr><w:shd w:fill=\"00FF00\"/><w:tcPrChange w:id=\"83\"/></w:tcPr><w:p/></w:tc>",
            "</w:tr></w:tbl>",
            "<w:sectPr><w:pgSz w:w=\"15840\"/><w:sectPrChange w:id=\"84\"/></w:sectPr>",
            "</w:body></w:document>"
        ),
        W_NS = W_NS
    );
    let mut document = parse(&xml);
    let outcomes = document.reject_all_revisions();
    assert_eq!(outcomes.len(), 5);
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));
    let saved = document_to_xml(&document);
    assert!(!saved.contains("PrChange"));
    assert!(!saved.contains("w:pStyle"));
    assert!(!saved.contains("w:tblStyle"));
    assert!(!saved.contains("w:tblHeader"));
    assert!(!saved.contains("w:shd"));
    assert!(!saved.contains("w:pgSz"));
    assert!(saved.contains("<w:sectPr/>"));
}
