use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Inline, PropertyScope, RevisionCategory, RevisionTarget};
use docxcore::review::RevisionOutcome;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const PROPERTY_FIXTURE: &str = include_str!("fixtures/property-changes.xml");

fn parse(xml: &str) -> docxcore::model::Document {
    parse_document_xml(xml, &Relationships::default())
}

fn inline_document() -> docxcore::model::Document {
    parse(&format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:r><w:t>A</w:t></w:r>",
            "<w:ins w:id=\"1\" w:author=\"Ada\"><w:r><w:t>X</w:t></w:r></w:ins>",
            "<w:r><w:t>B</w:t></w:r>",
            "<w:del w:id=\"2\"><w:r><w:delText>D</w:delText></w:r></w:del>",
            "</w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    ))
}

fn mixed_document() -> docxcore::model::Document {
    parse(&format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body>",
            "<w:p><w:r><w:t>A</w:t></w:r>",
            "<w:ins w:id=\"1\"><w:r><w:t>X</w:t></w:r></w:ins></w:p>",
            "<w:p><w:pPr><w:jc w:val=\"center\"/>",
            "<w:pPrChange w:id=\"2\"><w:pPr><w:jc w:val=\"left\"/></w:pPr></w:pPrChange>",
            "</w:pPr><w:r><w:rPr><w:b/>",
            "<w:rPrChange w:id=\"3\"><w:rPr><w:i/></w:rPr></w:rPrChange>",
            "</w:rPr><w:t>B</w:t></w:r></w:p>",
            "<w:tbl><w:tblPr><w:tblStyle w:val=\"CurrentTable\"/>",
            "<w:tblPrChange w:id=\"4\"><w:tblPr><w:tblStyle w:val=\"OldTable\"/></w:tblPr></w:tblPrChange>",
            "</w:tblPr><w:tr><w:trPr><w:tblHeader/>",
            "<w:trPrChange w:id=\"5\"><w:trPr><w:cantSplit/></w:trPr></w:trPrChange>",
            "</w:trPr><w:tc><w:tcPr><w:shd w:fill=\"00FF00\"/>",
            "<w:tcPrChange w:id=\"6\"><w:tcPr><w:shd w:fill=\"FFFF00\"/></w:tcPr></w:tcPrChange>",
            "</w:tcPr><w:p><w:r><w:t>C</w:t></w:r>",
            "<w:del w:id=\"7\"><w:r><w:delText>Y</w:delText></w:r></w:del>",
            "</w:p></w:tc></w:tr></w:tbl>",
            "</w:body></w:document>"
        ),
        W_NS = W_NS
    ))
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

#[test]
fn editor_enumerates_and_navigates_mixed_revision_scopes_in_source_order() {
    let mut editor = Editor::new(mixed_document());
    let locations = editor.revision_locations();
    assert_eq!(
        locations
            .iter()
            .map(|location| location.address.metadata.id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["1", "2", "3", "4", "5", "6", "7"]
    );
    assert_eq!(
        locations
            .iter()
            .map(|location| location.address.category.clone())
            .collect::<Vec<_>>(),
        [
            RevisionCategory::Inline(docxcore::model::RevisionKind::Insert),
            RevisionCategory::Property(PropertyScope::Paragraph),
            RevisionCategory::Property(PropertyScope::Run),
            RevisionCategory::Property(PropertyScope::Table),
            RevisionCategory::Property(PropertyScope::TableRow),
            RevisionCategory::Property(PropertyScope::TableCell),
            RevisionCategory::Inline(docxcore::model::RevisionKind::Delete),
        ]
    );
    assert_eq!(locations[0].start, Caret::top(0, 1));
    assert_eq!(locations[1].start, Caret::top(1, 0));
    assert_eq!(locations[2].end, Caret::top(1, 1));
    for location in &locations[3..] {
        assert_eq!(location.start.path, vec![2, 0, 0, 0]);
    }

    assert_eq!(
        editor
            .next_revision()
            .unwrap()
            .address
            .metadata
            .id
            .as_deref(),
        Some("1")
    );
    for id in ["2", "3", "4", "5", "6", "7"] {
        assert_eq!(
            editor
                .next_revision()
                .unwrap()
                .address
                .metadata
                .id
                .as_deref(),
            Some(id),
            "navigation must distinguish revisions sharing one caret boundary"
        );
    }
    assert_eq!(
        editor
            .next_revision()
            .unwrap()
            .address
            .metadata
            .id
            .as_deref(),
        Some("1"),
        "next wraps"
    );
    assert_eq!(
        editor
            .previous_revision()
            .unwrap()
            .address
            .metadata
            .id
            .as_deref(),
        Some("7"),
        "previous wraps"
    );
}

#[test]
fn current_action_preserves_selection_and_has_exact_undo_and_redo() {
    let mut editor = Editor::new(inline_document());
    let insertion = target_with_id(&editor, "1");
    editor.select_revision(insertion).unwrap();
    editor.anchor = Some(Caret::top(0, 0));
    let original = editor.doc.clone();
    let original_caret = editor.caret.clone();
    let original_anchor = editor.anchor.clone();

    assert!(matches!(
        editor.accept_current_revision(),
        Some(RevisionOutcome::Applied { target, .. }) if target == insertion
    ));
    assert_eq!(editor.doc.plain_text(), "AXBD\n");
    assert_eq!(editor.caret, original_caret);
    assert_eq!(editor.anchor, original_anchor);
    assert!(editor.current_revision().is_none());
    let accepted = editor.doc.clone();
    let accepted_caret = editor.caret.clone();
    let accepted_anchor = editor.anchor.clone();

    assert!(editor.undo());
    assert_eq!(editor.doc, original);
    assert_eq!(editor.caret, original_caret);
    assert_eq!(editor.anchor, original_anchor);
    assert!(!editor.undo(), "one review action creates one checkpoint");

    assert!(editor.redo());
    assert_eq!(editor.doc, accepted);
    assert_eq!(editor.caret, accepted_caret);
    assert_eq!(editor.anchor, accepted_anchor);
    assert!(!editor.redo());
}

#[test]
fn accept_all_is_one_transaction_and_keeps_table_carets_valid() {
    let mut editor = Editor::new(mixed_document());
    editor.caret = Caret::at(vec![2, 0, 0, 0], 1);
    editor.anchor = Some(Caret::top(0, 0));
    let original = editor.doc.clone();
    let original_caret = editor.caret.clone();
    let original_anchor = editor.anchor.clone();

    let outcomes = editor.accept_all_revisions();
    assert_eq!(outcomes.len(), 7);
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));
    assert!(editor.doc.revisions().is_empty());
    assert_eq!(editor.caret, original_caret);
    assert_eq!(editor.anchor, original_anchor);

    let accepted = editor.doc.clone();
    assert!(editor.undo());
    assert_eq!(editor.doc, original);
    assert_eq!(editor.caret, original_caret);
    assert_eq!(editor.anchor, original_anchor);
    assert!(!editor.undo(), "accept-all is a single checkpoint");
    assert!(editor.redo());
    assert_eq!(editor.doc, accepted);
    assert_eq!(editor.caret.path, vec![2, 0, 0, 0]);
}

#[test]
fn reject_current_and_reject_all_are_single_history_transactions() {
    let mut current = Editor::new(inline_document());
    let deletion = target_with_id(&current, "2");
    current.select_revision(deletion).unwrap();
    assert!(matches!(
        current.reject_current_revision(),
        Some(RevisionOutcome::Applied { target, .. }) if target == deletion
    ));
    assert_eq!(current.doc.revisions().len(), 1);
    assert!(current.undo());
    assert_eq!(current.doc.revisions().len(), 2);
    assert!(!current.undo(), "reject-current is one checkpoint");
    assert!(current.redo());
    assert_eq!(current.doc.revisions().len(), 1);

    let mut all = Editor::new(mixed_document());
    let original = all.doc.clone();
    let outcomes = all.reject_all_revisions();
    assert_eq!(outcomes.len(), 7);
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));
    assert!(all.doc.revisions().is_empty());
    let rejected = all.doc.clone();
    assert!(all.undo());
    assert_eq!(all.doc, original);
    assert!(!all.undo(), "reject-all is one checkpoint");
    assert!(all.redo());
    assert_eq!(all.doc, rejected);
}

#[test]
fn rejecting_a_table_property_keeps_the_selected_table_path_and_round_trips_history() {
    let mut editor = Editor::new(mixed_document());
    let cell_change = target_with_id(&editor, "6");
    let location = editor.select_revision(cell_change).unwrap();
    assert_eq!(location.start.path, vec![2, 0, 0, 0]);
    editor.caret.offset = 1;
    editor.anchor = Some(Caret::at(vec![2, 0, 0, 0], 0));
    let original = editor.doc.clone();

    assert!(editor.reject_revision(cell_change).is_applied());
    assert_eq!(editor.caret, Caret::at(vec![2, 0, 0, 0], 1));
    assert_eq!(editor.anchor, Some(Caret::at(vec![2, 0, 0, 0], 0)));
    let Block::Table(table) = &editor.doc.body[2] else {
        panic!("table")
    };
    assert!(
        table.rows[0].cells[0]
            .raw_tcpr
            .as_deref()
            .unwrap()
            .contains("FFFF00")
    );

    assert!(editor.undo());
    assert_eq!(editor.doc, original);
    assert!(editor.redo());
    assert_eq!(editor.caret.path, vec![2, 0, 0, 0]);
}

#[test]
fn unsupported_and_stale_actions_are_noops_and_do_not_clear_redo() {
    let document = parse(&format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:moveFromRangeStart w:id=\"9\" w:name=\"move\"/>",
            "<w:r><w:t>A</w:t></w:r></w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    ));
    let mut editor = Editor::new(document);
    let unsupported = target_with_id(&editor, "9");
    editor.insert_char('Z');
    assert!(editor.undo());
    let before = editor.doc.clone();

    assert!(matches!(
        editor.accept_revision(unsupported),
        RevisionOutcome::Unsupported { .. }
    ));
    assert_eq!(editor.doc, before);
    assert!(editor.redo(), "a no-op review must preserve the redo stack");
    assert_eq!(editor.doc.plain_text(), "ZA\n");

    let mut stale = Editor::new(inline_document());
    let before = stale.doc.clone();
    assert!(matches!(
        stale.reject_revision(RevisionTarget(u64::MAX)),
        RevisionOutcome::Stale { .. }
    ));
    assert_eq!(stale.doc, before);
    assert!(!stale.undo(), "a stale action must not create history");

    let mut malformed = Editor::new(parse(PROPERTY_FIXTURE));
    let malformed_target = target_with_id(&malformed, "18");
    let before = malformed.doc.clone();
    assert!(matches!(
        malformed.reject_revision(malformed_target),
        RevisionOutcome::Malformed { .. }
    ));
    assert_eq!(malformed.doc, before);
    assert!(
        !malformed.undo(),
        "a malformed action must not create history"
    );
}

#[test]
fn ordinary_edits_at_revision_boundaries_leave_the_wrapper_untouched() {
    let document = parse(&format!(
        concat!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:p>",
            "<w:ins w:id=\"11\" w:author=\"Ada\"><w:r><w:t>tracked</w:t></w:r></w:ins>",
            "<w:r><w:t>AB</w:t></w:r></w:p></w:body></w:document>"
        ),
        W_NS = W_NS
    ));
    let mut editor = Editor::new(document);
    let target = target_with_id(&editor, "11");
    let (raw, metadata) = match &editor.doc.body[0] {
        Block::Paragraph(paragraph) => match &paragraph.content[0] {
            Inline::Revision { raw, metadata, .. } => (raw.clone(), metadata.clone()),
            _ => panic!("revision"),
        },
        _ => panic!("paragraph"),
    };

    editor.insert_char('Z');
    editor.move_end();
    editor.backspace();
    editor.anchor = Some(Caret::top(0, 0));
    editor.caret = Caret::top(0, 1);
    assert!(editor.delete_selection());

    assert_eq!(editor.doc.revisions().len(), 1);
    assert_eq!(editor.doc.revisions()[0].target, target);
    let Block::Paragraph(paragraph) = &editor.doc.body[0] else {
        panic!("paragraph")
    };
    let Inline::Revision {
        raw: after_raw,
        metadata: after_metadata,
        content_changed,
        ..
    } = &paragraph.content[0]
    else {
        panic!("ordinary edits consumed the revision wrapper")
    };
    assert_eq!(after_raw, &raw);
    assert_eq!(after_metadata, &metadata);
    assert!(!content_changed, "ordinary edits rewrote the wrapper");
}
