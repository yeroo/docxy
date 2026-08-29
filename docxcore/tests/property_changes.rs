use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{
    Block, Inline, PropertyScope, PropertySnapshot, PropertyState, RevisionCategory,
};
use docxcore::serialize::document_to_xml;

const FIXTURE: &str = include_str!("fixtures/property-changes.xml");

fn parsed_fixture() -> docxcore::model::Document {
    parse_document_xml(FIXTURE, &Relationships::default())
}

#[test]
fn parses_current_and_prior_state_for_every_property_scope() {
    let document = parsed_fixture();
    let scopes = document
        .revisions()
        .into_iter()
        .filter_map(|revision| match revision.category {
            RevisionCategory::Property(scope) => Some(scope),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        scopes,
        vec![
            PropertyScope::Section,
            PropertyScope::Paragraph,
            PropertyScope::Run,
            PropertyScope::Run,
            PropertyScope::Run,
            PropertyScope::Run,
            PropertyScope::Table,
            PropertyScope::TableRow,
            PropertyScope::TableCell,
        ]
    );

    let Block::Paragraph(paragraph) = &document.body[0] else {
        panic!("fixture begins with a paragraph")
    };
    assert_eq!(
        paragraph.props.style_id.as_deref(),
        Some("CurrentParagraph")
    );
    let paragraph_change = paragraph.props.property_change.as_ref().unwrap();
    assert_eq!(paragraph_change.metadata.id.as_deref(), Some("12"));
    assert_eq!(
        paragraph_change.metadata.unknown_attributes,
        vec![("w:producer".to_string(), "paragraph-metadata".to_string())]
    );
    let PropertySnapshot::Present(PropertyState::Paragraph(previous)) = &paragraph_change.previous
    else {
        panic!("paragraph prior state is semantic")
    };
    assert_eq!(previous.style_id.as_deref(), Some("PreviousParagraph"));
    assert_eq!(previous.align, docxcore::model::Align::Right);
    assert!(!previous.rtl, "explicit false toggle remains false");
    assert!(
        previous
            .raw_props
            .iter()
            .any(|raw| raw.contains("w:keepLines"))
    );
    assert!(
        previous
            .raw_props
            .iter()
            .any(|raw| raw.contains("<w:bidi w:val=\"0\""))
    );

    let section_change = paragraph.props.section_property_change.as_ref().unwrap();
    let PropertySnapshot::Present(PropertyState::Section(previous_section)) =
        &section_change.previous
    else {
        panic!("section prior state is scoped XML")
    };
    assert!(previous_section.contains("w:w=\"11906\""));
    assert!(previous_section.contains("w:vendorSect"));
    assert!(
        !paragraph
            .props
            .section_break
            .as_deref()
            .unwrap()
            .contains("sectPrChange")
    );

    let Inline::Run(run) = &paragraph.content[0] else {
        panic!("fixture paragraph contains a run")
    };
    assert!(
        !run.props.bold,
        "current explicit false toggle remains false"
    );
    assert!(run.props.italic);
    assert_eq!(run.props.style_id.as_deref(), Some("CurrentCharacter"));
    assert!(
        run.props
            .raw_props
            .iter()
            .any(|raw| raw.contains("<w:b w:val=\"false\""))
    );
    let run_change = run.props.property_change.as_ref().unwrap();
    assert_eq!(run_change.metadata.author.as_deref(), Some("Run & Author"));
    assert!(run_change.raw.contains("<w:vendorChange"));
    let PropertySnapshot::Present(PropertyState::Run(previous)) = &run_change.previous else {
        panic!("run prior state is semantic")
    };
    assert!(previous.bold);
    assert!(!previous.italic);
    assert!(previous.underline);
    assert_eq!(previous.style_id.as_deref(), Some("PreviousCharacter"));
    assert!(previous.raw_props.iter().any(|raw| raw.contains("w:lang")));
    assert!(
        previous
            .raw_props
            .iter()
            .any(|raw| raw.contains("<w:i w:val=\"off\""))
    );

    let Block::Table(table) = &document.body[2] else {
        panic!("fixture ends with a table")
    };
    for (change, expected_scope, expected_text) in [
        (
            table.property_change.as_ref().unwrap(),
            PropertyScope::Table,
            "PreviousTable",
        ),
        (
            table.rows[0].property_change.as_ref().unwrap(),
            PropertyScope::TableRow,
            "w:cantSplit",
        ),
        (
            table.rows[0].cells[0].property_change.as_ref().unwrap(),
            PropertyScope::TableCell,
            "FFFF00",
        ),
    ] {
        let PropertySnapshot::Present(state) = &change.previous else {
            panic!("raw property scope has a prior state")
        };
        assert_eq!(state.scope(), expected_scope);
        assert!(state.raw_xml().unwrap().contains(expected_text));
        assert!(change.raw.contains("<w:vendorChange"));
    }
    assert!(!table.raw_tblpr.as_deref().unwrap().contains("tblPrChange"));
    assert!(!table.rows[0].raw_props[0].contains("trPrChange"));
    assert!(
        !table.rows[0].cells[0]
            .raw_tcpr
            .as_deref()
            .unwrap()
            .contains("tcPrChange")
    );
}

#[test]
fn absent_empty_and_malformed_snapshots_are_distinct_and_do_not_panic() {
    let document = parsed_fixture();
    let Block::Paragraph(paragraph) = &document.body[1] else {
        panic!("second fixture block is a paragraph")
    };

    let snapshots = paragraph
        .content
        .iter()
        .map(|inline| {
            let Inline::Run(run) = inline else {
                panic!("fixture contains only runs")
            };
            &run.props.property_change.as_ref().unwrap().previous
        })
        .collect::<Vec<_>>();
    assert!(matches!(snapshots[0], PropertySnapshot::Absent));
    assert!(matches!(snapshots[1], PropertySnapshot::Malformed(raw) if raw.contains("notRPr")));
    assert!(matches!(
        snapshots[2],
        PropertySnapshot::Present(PropertyState::Run(previous))
            if **previous == docxcore::model::RunProps::default()
    ));
}

#[test]
fn fixture_parse_save_parse_is_stable_and_schema_ordered() {
    let document = parsed_fixture();
    let saved = document_to_xml(&document);

    assert_eq!(saved.matches("<w:rPrChange").count(), 4);
    for name in [
        "pPrChange",
        "tblPrChange",
        "trPrChange",
        "tcPrChange",
        "sectPrChange",
    ] {
        assert_eq!(saved.matches(&format!("<w:{name}")).count(), 1, "{name}");
    }
    assert_eq!(saved.matches("CurrentTable").count(), 1);
    assert_eq!(saved.matches("PreviousTable").count(), 1);
    assert!(
        saved.find("w:lang w:val=\"en-US\"").unwrap()
            < saved.find("<w:rPrChange w:id=\"11\"").unwrap()
    );
    assert!(saved.find("<w:sectPrChange").unwrap() < saved.find("<w:pPrChange").unwrap());
    assert!(saved.find("w:fill=\"ABCDEF\"").unwrap() < saved.find("<w:tblPrChange").unwrap());
    assert!(saved.find("<w:tblHeader").unwrap() < saved.find("<w:trPrChange").unwrap());
    assert!(saved.find("w:fill=\"00FF00\"").unwrap() < saved.find("<w:tcPrChange").unwrap());

    let reparsed = parse_document_xml(&saved, &Relationships::default());
    let saved_again = document_to_xml(&reparsed);
    let reparsed_again = parse_document_xml(&saved_again, &Relationships::default());
    assert_eq!(reparsed_again, reparsed);
}

#[test]
fn removing_modeled_changes_does_not_leave_raw_duplicates() {
    let mut document = parsed_fixture();
    let Block::Paragraph(paragraph) = &mut document.body[0] else {
        panic!("fixture begins with a paragraph")
    };
    paragraph.props.property_change = None;
    paragraph.props.section_property_change = None;
    let Inline::Run(run) = &mut paragraph.content[0] else {
        panic!("fixture paragraph contains a run")
    };
    run.props.property_change = None;

    let Block::Paragraph(edge_cases) = &mut document.body[1] else {
        panic!("fixture second block is a paragraph")
    };
    for inline in &mut edge_cases.content {
        let Inline::Run(run) = inline else {
            panic!("fixture contains only runs")
        };
        run.props.property_change = None;
    }

    let Block::Table(table) = &mut document.body[2] else {
        panic!("fixture ends with a table")
    };
    table.property_change = None;
    table.rows[0].property_change = None;
    table.rows[0].cells[0].property_change = None;

    let saved = document_to_xml(&document);
    assert!(!saved.contains("PrChange"));
    assert_eq!(saved.matches("CurrentTable").count(), 1);
    assert_eq!(saved.matches("w:fill=\"ABCDEF\"").count(), 1);
    assert_eq!(saved.matches("w:fill=\"00FF00\"").count(), 1);
}
