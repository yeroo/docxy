//! Tracked insertions/deletions of paragraph marks (`w:pPr/w:rPr/w:ins|w:del`).

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, RevisionCategory, RevisionKind, RevisionTarget};
use docxcore::package::{load_package, new_package, save_package};
use docxcore::review::RevisionOutcome;
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn parse(xml: &str) -> Document {
    parse_document_xml(xml, &Relationships::default())
}

fn doc(body: &str) -> Document {
    parse(&format!(
        "<w:document xmlns:w=\"{W_NS}\"><w:body>{body}<w:sectPr/></w:body></w:document>"
    ))
}

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

/// A paragraph whose mark is tracked (`ins` or `del`), with a centered style
/// marker so tests can tell which paragraph's properties survive a merge.
fn marked(text: &str, kind: &str, id: &str, style: &str) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"{style}\"/><w:rPr><w:b/><w:{kind} w:id=\"{id}\" \
         w:author=\"Ada\" w:date=\"2026-01-01T00:00:00Z\"/></w:rPr></w:pPr>\
         <w:r><w:t>{text}</w:t></w:r></w:p>"
    )
}

fn texts(document: &Document) -> Vec<String> {
    document
        .body
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(p) => Some(p.plain_text()),
            _ => None,
        })
        .collect()
}

fn style_of(document: &Document, index: usize) -> Option<String> {
    match &document.body[index] {
        Block::Paragraph(p) => p.props.style_id.clone(),
        _ => panic!("paragraph"),
    }
}

fn target_with_id(document: &Document, id: &str) -> RevisionTarget {
    document
        .revisions()
        .into_iter()
        .find(|revision| revision.metadata.id.as_deref() == Some(id))
        .unwrap_or_else(|| panic!("revision id {id}"))
        .target
}

#[test]
fn paragraph_marks_are_listed_after_the_paragraph_content() {
    let document = doc(&format!(
        "<w:p><w:pPr><w:rPr><w:ins w:id=\"7\" w:author=\"Ada\" w:date=\"2026-01-01T00:00:00Z\"/>\
         </w:rPr></w:pPr><w:ins w:id=\"6\"><w:r><w:t>new</w:t></w:r></w:ins></w:p>{}{}",
        marked("gone", "del", "8", "S2"),
        para("tail")
    ));
    let revisions = document.revisions();
    let categories = revisions
        .iter()
        .map(|r| (r.metadata.id.clone().unwrap(), r.category.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        categories,
        [
            ("6".into(), RevisionCategory::Inline(RevisionKind::Insert)),
            (
                "7".into(),
                RevisionCategory::ParagraphMark(RevisionKind::Insert)
            ),
            (
                "8".into(),
                RevisionCategory::ParagraphMark(RevisionKind::Delete)
            ),
        ]
    );
    assert_eq!(revisions[1].metadata.author.as_deref(), Some("Ada"));
    assert_eq!(
        revisions[1].metadata.date.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    let unique = revisions
        .iter()
        .map(|r| r.target)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), 3, "targets are unique");
}

#[test]
fn accepting_a_deleted_mark_merges_with_the_next_paragraph_using_its_props() {
    let mut document = doc(&format!(
        "{}<w:p><w:pPr><w:pStyle w:val=\"Next\"/></w:pPr><w:r><w:t>B</w:t></w:r></w:p>{}",
        marked("A", "del", "1", "First"),
        para("C")
    ));
    assert!(
        document
            .accept_revision(target_with_id(&document, "1"))
            .is_applied()
    );
    assert_eq!(texts(&document), ["AB", "C"]);
    assert_eq!(style_of(&document, 0).as_deref(), Some("Next"));
    assert!(document.revisions().is_empty());
}

#[test]
fn rejecting_a_deleted_mark_keeps_the_paragraph_and_its_mark_formatting() {
    let mut document = doc(&format!(
        "{}{}",
        marked("A", "del", "1", "First"),
        para("B")
    ));
    assert!(
        document
            .reject_revision(target_with_id(&document, "1"))
            .is_applied()
    );
    assert_eq!(texts(&document), ["A", "B"]);
    assert_eq!(style_of(&document, 0).as_deref(), Some("First"));
    let xml = document_to_xml(&document);
    assert!(xml.contains("<w:rPr><w:b/></w:rPr>"), "{xml}");
    assert!(!xml.contains("<w:del"), "{xml}");
}

#[test]
fn inserted_marks_merge_on_reject_and_stay_on_accept() {
    let body = format!("{}{}", marked("A", "ins", "1", "First"), para("B"));
    let mut rejected = doc(&body);
    assert!(
        rejected
            .reject_revision(target_with_id(&rejected, "1"))
            .is_applied()
    );
    assert_eq!(texts(&rejected), ["AB"]);
    assert_eq!(style_of(&rejected, 0), None, "the next paragraph's props");

    let mut accepted = doc(&body);
    assert!(
        accepted
            .accept_revision(target_with_id(&accepted, "1"))
            .is_applied()
    );
    assert_eq!(texts(&accepted), ["A", "B"]);
    assert_eq!(style_of(&accepted, 0).as_deref(), Some("First"));
}

#[test]
fn a_mark_with_no_following_paragraph_only_drops_the_record() {
    // Last paragraph of the body (the trailing sectPr is not a paragraph), and
    // a paragraph followed by a table.
    let mut last = doc(&format!("{}{}", para("A"), marked("B", "del", "1", "S")));
    assert!(
        last.accept_revision(target_with_id(&last, "1"))
            .is_applied()
    );
    assert_eq!(texts(&last), ["A", "B"]);
    assert!(last.revisions().is_empty());

    let table = "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
    let mut before_table = doc(&format!(
        "{}{table}{}",
        marked("A", "del", "1", "S"),
        para("Z")
    ));
    assert!(
        before_table
            .accept_revision(target_with_id(&before_table, "1"))
            .is_applied()
    );
    assert_eq!(texts(&before_table), ["A", "Z"]);
    assert!(matches!(before_table.body[1], Block::Table(_)));
}

#[test]
fn marks_inside_table_cells_merge_within_the_cell() {
    let mut document = doc(&format!(
        "<w:tbl><w:tr><w:tc>{}{}</w:tc></w:tr></w:tbl>{}",
        marked("A", "del", "1", "S"),
        para("B"),
        para("after")
    ));
    assert!(
        document
            .accept_revision(target_with_id(&document, "1"))
            .is_applied()
    );
    let Block::Table(table) = &document.body[0] else {
        panic!("table")
    };
    let cell = &table.rows[0].cells[0];
    assert_eq!(cell.blocks.len(), 1);
    assert_eq!(cell.blocks[0].plain_text(), "AB");
    assert_eq!(texts(&document), ["after"]);
}

#[test]
fn accept_and_reject_all_handle_marks_mixed_with_inline_revisions() {
    // Original: "Keep" / "Old one" / "Tail". Revised: "Keep" / "New" / "Tail"
    // where "New" is an inserted paragraph and "Old one" a deleted one, with
    // an inline insertion inside the kept first paragraph too.
    let body = concat!(
        "<w:p><w:r><w:t>Keep</w:t></w:r><w:ins w:id=\"1\"><w:r><w:t>!</w:t></w:r></w:ins></w:p>",
        "<w:p><w:pPr><w:rPr><w:del w:id=\"2\"/></w:rPr></w:pPr>",
        "<w:del w:id=\"3\"><w:r><w:delText>Old</w:delText></w:r></w:del>",
        "<w:r><w:t xml:space=\"preserve\"> </w:t></w:r>",
        "<w:del w:id=\"4\"><w:r><w:delText>one</w:delText></w:r></w:del></w:p>",
        "<w:p><w:pPr><w:rPr><w:ins w:id=\"5\"/></w:rPr></w:pPr>",
        "<w:ins w:id=\"6\"><w:r><w:t>New</w:t></w:r></w:ins></w:p>",
        "<w:p><w:r><w:t>Tail</w:t></w:r></w:p>"
    );
    let mut accepted = doc(body);
    let outcomes = accepted.accept_all_revisions();
    assert_eq!(outcomes.len(), 6);
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));
    assert_eq!(texts(&accepted), ["Keep!", " New", "Tail"]);
    assert!(accepted.revisions().is_empty());

    let mut rejected = doc(body);
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(RevisionOutcome::is_applied)
    );
    assert_eq!(texts(&rejected), ["Keep", "Old one", "Tail"]);
    assert!(rejected.revisions().is_empty());
    let xml = document_to_xml(&rejected);
    assert!(!xml.contains("<w:ins") && !xml.contains("<w:del"), "{xml}");
}

#[test]
fn untouched_marks_round_trip_stably_through_save_and_load() {
    let document = doc(&format!(
        "{}{}{}",
        marked("A", "del", "1", "S1"),
        marked("B", "ins", "2", "S2"),
        para("C")
    ));
    let first = document_to_xml(&document);
    assert!(first.contains(
        "<w:rPr><w:b/><w:del w:id=\"1\" w:author=\"Ada\" w:date=\"2026-01-01T00:00:00Z\"/></w:rPr>"
    ));
    let second = document_to_xml(&parse(&first));
    assert_eq!(first, second, "load -> save is idempotent");

    let bytes = save_package(&new_package(document.clone()));
    let reloaded = load_package(&bytes).expect("reload").document;
    assert_eq!(document_to_xml(&reloaded), first);
    assert_eq!(reloaded.revisions().len(), 2);
}

#[test]
fn rejecting_a_paragraph_property_change_keeps_the_mark_revision() {
    let mut document = doc(concat!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/><w:rPr><w:del w:id=\"1\"/></w:rPr>",
        "<w:pPrChange w:id=\"2\"><w:pPr/></w:pPrChange></w:pPr>",
        "<w:r><w:t>A</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>B</w:t></w:r></w:p>"
    ));
    assert!(
        document
            .reject_revision(target_with_id(&document, "2"))
            .is_applied()
    );
    let revisions = document.revisions();
    assert_eq!(revisions.len(), 1);
    assert_eq!(
        revisions[0].category,
        RevisionCategory::ParagraphMark(RevisionKind::Delete)
    );
    assert!(document_to_xml(&document).contains("<w:rPr><w:del w:id=\"1\"/></w:rPr>"));
}

#[test]
fn editor_navigates_to_a_mark_at_the_paragraph_end_and_accept_all_is_one_step() {
    let mut editor = Editor::new(doc(&format!(
        "{}{}{}",
        para("Intro"),
        marked("Gone", "del", "1", "S"),
        para("Next")
    )));
    let location = editor.next_revision().expect("a revision");
    assert_eq!(
        location.address.category,
        RevisionCategory::ParagraphMark(RevisionKind::Delete)
    );
    assert_eq!(location.start, Caret::at(vec![1], 4), "end of 'Gone'");
    assert_eq!(location.end, location.start);

    // Accept the revision at the caret without navigation state.
    let mut at_caret = Editor::new(editor.doc.clone());
    at_caret.caret = Caret::at(vec![1], 4);
    assert!(at_caret.accept_current_revision().unwrap().is_applied());
    assert_eq!(texts(&at_caret.doc), ["Intro", "GoneNext"]);
    assert!(at_caret.undo());
    assert_eq!(texts(&at_caret.doc), ["Intro", "Gone", "Next"]);

    let outcomes = editor.accept_all_revisions();
    assert!(outcomes.iter().all(RevisionOutcome::is_applied));
    assert_eq!(texts(&editor.doc), ["Intro", "GoneNext"]);
    assert!(editor.undo());
    assert!(!editor.undo(), "accept-all is a single checkpoint");
}

/// A paragraph inserted by one reviewer and deleted by another: its mark rPr
/// holds both an `ins` and a `del`.
fn inserted_then_deleted() -> Document {
    doc(concat!(
        "<w:p><w:pPr><w:rPr><w:ins w:id=\"1\" w:author=\"Ada\"/><w:del w:id=\"2\" w:author=\"Bo\"/>",
        "</w:rPr></w:pPr><w:r><w:t>X</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>Y</w:t></w:r></w:p>"
    ))
}

#[test]
fn a_mark_with_both_an_insertion_and_a_deletion_lists_and_acts_on_each() {
    let document = inserted_then_deleted();
    let revisions = document.revisions();
    let listed: Vec<(String, RevisionCategory)> = revisions
        .iter()
        .map(|r| (r.metadata.id.clone().unwrap(), r.category.clone()))
        .collect();
    assert_eq!(
        listed,
        [
            (
                "1".into(),
                RevisionCategory::ParagraphMark(RevisionKind::Insert)
            ),
            (
                "2".into(),
                RevisionCategory::ParagraphMark(RevisionKind::Delete)
            ),
        ]
    );
    assert_ne!(revisions[0].target, revisions[1].target);
    let first = document_to_xml(&document);
    assert_eq!(document_to_xml(&parse(&first)), first, "round trip");

    // Accepting the insertion keeps the mark and only its own record.
    let mut accepted_ins = inserted_then_deleted();
    assert!(
        accepted_ins
            .accept_revision(target_with_id(&accepted_ins, "1"))
            .is_applied()
    );
    assert_eq!(texts(&accepted_ins), ["X", "Y"]);
    let xml = document_to_xml(&accepted_ins);
    assert!(
        xml.contains("<w:del w:id=\"2\"") && !xml.contains("<w:ins w:id=\"1\""),
        "{xml}"
    );
    assert_eq!(accepted_ins.revisions().len(), 1);

    // Rejecting the deletion keeps the mark and the insertion record.
    let mut rejected_del = inserted_then_deleted();
    assert!(
        rejected_del
            .reject_revision(target_with_id(&rejected_del, "2"))
            .is_applied()
    );
    assert_eq!(texts(&rejected_del), ["X", "Y"]);
    let xml = document_to_xml(&rejected_del);
    assert!(
        xml.contains("<w:ins w:id=\"1\"") && !xml.contains("<w:del w:id=\"2\""),
        "{xml}"
    );

    // Accepting the deletion removes the mark, and with it the insertion.
    let mut accepted_del = inserted_then_deleted();
    assert!(
        accepted_del
            .accept_revision(target_with_id(&accepted_del, "2"))
            .is_applied()
    );
    assert_eq!(texts(&accepted_del), ["XY"]);
    assert!(accepted_del.revisions().is_empty());
}

#[test]
fn accept_all_and_reject_all_both_remove_an_inserted_then_deleted_mark() {
    for accept in [true, false] {
        let mut document = inserted_then_deleted();
        let outcomes = if accept {
            document.accept_all_revisions()
        } else {
            document.reject_all_revisions()
        };
        assert_eq!(outcomes.len(), 2);
        assert!(
            outcomes.iter().all(RevisionOutcome::is_applied),
            "accept={accept}: {outcomes:?}"
        );
        assert_eq!(texts(&document), ["XY"], "accept={accept}");
        let xml = document_to_xml(&document);
        assert!(!xml.contains("<w:ins") && !xml.contains("<w:del"), "{xml}");
    }
}

#[test]
fn removing_a_section_ending_mark_merges_the_section_away_as_word_does() {
    // The merged paragraph takes the next paragraph's properties, so its
    // content joins the following section (whose sectPr comes later).
    let mut document = doc(concat!(
        "<w:p><w:pPr><w:rPr><w:del w:id=\"1\"/></w:rPr><w:sectPr><w:pgSz w:w=\"11906\"/></w:sectPr>",
        "</w:pPr><w:r><w:t>A</w:t></w:r></w:p>",
        "<w:p><w:r><w:t>B</w:t></w:r></w:p>"
    ));
    assert!(
        document
            .accept_revision(target_with_id(&document, "1"))
            .is_applied()
    );
    assert_eq!(texts(&document), ["AB"]);
    let Block::Paragraph(merged) = &document.body[0] else {
        panic!("paragraph")
    };
    assert_eq!(merged.props.section_break, None);
}

/// A centered paragraph with a tracked pPrChange (previously unformatted) and
/// a tracked mark record of `kind`, followed by "B".
fn property_change_and_mark(kind: &str) -> Document {
    doc(&format!(
        "<w:p><w:pPr><w:jc w:val=\"center\"/><w:rPr><w:{kind} w:id=\"1\"/></w:rPr>\
         <w:pPrChange w:id=\"2\"><w:pPr/></w:pPrChange></w:pPr>\
         <w:r><w:t>A</w:t></w:r></w:p><w:p><w:r><w:t>B</w:t></w:r></w:p>"
    ))
}

#[test]
fn bulk_actions_apply_a_paragraphs_property_change_before_merging_it_away() {
    use docxcore::model::Align;
    let align = |d: &Document| match &d.body[0] {
        Block::Paragraph(p) => p.props.align,
        _ => panic!("paragraph"),
    };
    // (mark kind, accept?) -> expected paragraphs and first alignment.
    for (kind, accept, expected, centered) in [
        ("del", true, vec!["AB"], false),
        ("del", false, vec!["A", "B"], false),
        ("ins", true, vec!["A", "B"], true),
        ("ins", false, vec!["AB"], false),
    ] {
        let mut document = property_change_and_mark(kind);
        let outcomes = if accept {
            document.accept_all_revisions()
        } else {
            document.reject_all_revisions()
        };
        assert_eq!(outcomes.len(), 2);
        assert!(
            outcomes.iter().all(RevisionOutcome::is_applied),
            "{kind} accept={accept}: {outcomes:?}"
        );
        assert_eq!(texts(&document), expected, "{kind} accept={accept}");
        assert_eq!(
            align(&document) == Align::Center,
            centered,
            "{kind} accept={accept}"
        );
        assert!(document.revisions().is_empty());
    }
}

fn editor_with(body: &str) -> Editor {
    Editor::new(doc(body))
}

fn type_text(editor: &mut Editor, text: &str) {
    for c in text.chars() {
        editor.insert_char(c);
    }
}

fn mark_record_count(xml: &str) -> usize {
    xml.matches("<w:del w:id=\"1\"").count() + xml.matches("<w:ins w:id=\"1\"").count()
}

#[test]
fn enter_keeps_the_mark_record_on_the_paragraph_that_ends_with_the_mark() {
    // Enter at the end of a paragraph whose mark is deleted, then type: the
    // deleted mark is the one after "New", so Accept All joins "New" and
    // "World" and leaves "Hello" alone, as Word does.
    let mut editor = editor_with(&format!(
        "{}{}",
        marked("Hello", "del", "1", "S"),
        para("World")
    ));
    editor.caret = Caret::at(vec![0], 5);
    editor.insert_newline();
    type_text(&mut editor, "New");
    assert_eq!(editor.doc.revisions().len(), 1);
    assert_eq!(
        mark_record_count(&document_to_xml(&editor.doc)),
        1,
        "one record"
    );
    editor.accept_all_revisions();
    assert_eq!(texts(&editor.doc), ["Hello", "NewWorld"]);

    // A split in the middle: one record, on the second half.
    let mut editor = editor_with(&format!(
        "{}{}",
        marked("Hello", "del", "1", "S"),
        para("World")
    ));
    let before = editor.doc.clone();
    editor.caret = Caret::at(vec![0], 2);
    editor.insert_newline();
    assert_eq!(texts(&editor.doc), ["He", "llo", "World"]);
    let revisions = editor.doc.revisions();
    assert_eq!(revisions.len(), 1);
    let xml = document_to_xml(&editor.doc);
    assert_eq!(mark_record_count(&xml), 1, "{xml}");
    let Block::Paragraph(second) = &editor.doc.body[1] else {
        panic!("paragraph")
    };
    assert_eq!(second.props.mark_revisions.len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.doc, before, "undo restores the record");
}

#[test]
fn pasting_paragraphs_into_a_marked_paragraph_keeps_the_record_on_the_last() {
    use docxcore::editor::Clip;
    use docxcore::model::{Inline, Run, RunProps};
    let run = |t: &str| {
        vec![Inline::Run(Run {
            text: t.to_string(),
            props: RunProps::default(),
        })]
    };
    let mut editor = editor_with(&format!("{}{}", marked("AB", "ins", "1", "S"), para("Z")));
    editor.caret = Caret::at(vec![0], 1);
    editor.paste(&Clip {
        paras: vec![run("x"), run("y"), run("z")],
    });
    assert_eq!(texts(&editor.doc), ["Ax", "y", "zB", "Z"]);
    let holders: Vec<usize> = editor
        .doc
        .body
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            Block::Paragraph(p) if !p.props.mark_revisions.is_empty() => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(holders, [2]);
    assert_eq!(mark_record_count(&document_to_xml(&editor.doc)), 1);
}

#[test]
fn joining_paragraphs_keeps_the_later_marks_records() {
    // Backspace at the start of B joins A (deleted mark) into one paragraph
    // ending with B's (untracked) mark: no record survives.
    let mut editor = editor_with(&format!("{}{}", marked("A", "del", "1", "S"), para("B")));
    let before = editor.doc.clone();
    editor.caret = Caret::at(vec![1], 0);
    editor.backspace();
    assert_eq!(texts(&editor.doc), ["AB"]);
    assert!(editor.doc.revisions().is_empty());
    assert_eq!(mark_record_count(&document_to_xml(&editor.doc)), 0);
    assert!(editor.undo());
    assert_eq!(editor.doc, before);

    // Delete at the end of A joins B (inserted mark): B's record is kept.
    let mut editor = editor_with(&format!(
        "{}{}{}",
        para("A"),
        marked("B", "ins", "1", "S"),
        para("C")
    ));
    editor.caret = Caret::at(vec![0], 1);
    editor.delete_forward();
    assert_eq!(texts(&editor.doc), ["AB", "C"]);
    let revisions = editor.doc.revisions();
    assert_eq!(revisions.len(), 1);
    assert_eq!(
        revisions[0].category,
        RevisionCategory::ParagraphMark(RevisionKind::Insert)
    );
    let xml = document_to_xml(&editor.doc);
    assert_eq!(mark_record_count(&xml), 1, "{xml}");
    assert!(
        editor
            .reject_all_revisions()
            .iter()
            .all(RevisionOutcome::is_applied)
    );
    assert_eq!(texts(&editor.doc), ["ABC"]);
}

#[test]
fn bulk_actions_resolve_marks_inside_text_boxes_within_revisions() {
    // A text box inside a tracked insertion/deletion run, whose first
    // paragraph has a tracked mark: the mark acts before its wrapper.
    for (wrapper, mark) in [
        ("del", "del"),
        ("del", "ins"),
        ("ins", "del"),
        ("ins", "ins"),
    ] {
        let text = if wrapper == "del" { "w:delText" } else { "w:t" };
        let body = format!(
            "<w:p><w:r><w:t>host</w:t></w:r><w:{wrapper} w:id=\"9\"><w:r><w:pict><v:shape><v:textbox>\
             <w:txbxContent><w:p><w:pPr><w:rPr><w:{mark} w:id=\"1\"/></w:rPr></w:pPr>\
             <w:r><{text}>one</{text}></w:r></w:p><w:p><w:r><{text}>two</{text}></w:r></w:p>\
             </w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:{wrapper}></w:p>"
        );
        for accept in [true, false] {
            let mut document = doc(&body);
            assert_eq!(document.revisions().len(), 2, "{wrapper}/{mark}");
            let outcomes = if accept {
                document.accept_all_revisions()
            } else {
                document.reject_all_revisions()
            };
            assert!(
                outcomes.iter().all(RevisionOutcome::is_applied),
                "{wrapper}/{mark} accept={accept}: {outcomes:?}"
            );
            assert!(
                document.revisions().is_empty(),
                "{wrapper}/{mark} accept={accept}"
            );
        }
    }
}

#[test]
fn new_table_cells_and_a_table_split_keep_records_with_the_physical_mark() {
    use docxcore::table::AutoFit;
    // Tab in the last cell adds a row templated on it: its first paragraph
    // has a tracked mark, but the new cells' marks are new and untracked.
    let mut editor = editor_with(&format!(
        "<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>{}",
        marked("cell", "del", "1", "S"),
        para("after")
    ));
    editor.caret = Caret::at(vec![0, 0, 0, 0], 2);
    assert!(editor.table_tab_adds_row());
    assert!(editor.table_next_cell());
    let Block::Table(table) = &editor.doc.body[0] else {
        panic!("table")
    };
    assert_eq!(table.rows.len(), 2);
    let Block::Paragraph(new_cell) = &table.rows[1].cells[0].blocks[0] else {
        panic!("paragraph")
    };
    assert!(new_cell.props.mark_revisions.is_empty());
    assert_eq!(mark_record_count(&document_to_xml(&editor.doc)), 1);

    // A table inserted mid-paragraph: the record stays on the second half.
    let mut editor = editor_with(&format!(
        "{}{}",
        marked("HelloWorld", "del", "1", "S"),
        para("Z")
    ));
    editor.caret = Caret::at(vec![0], 5);
    editor.insert_table(1, 1, AutoFit::Default).unwrap();
    let holders: Vec<String> = editor
        .doc
        .body
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) if !p.props.mark_revisions.is_empty() => Some(p.plain_text()),
            _ => None,
        })
        .collect();
    assert_eq!(holders, ["World"]);
    assert_eq!(mark_record_count(&document_to_xml(&editor.doc)), 1);
}

#[test]
fn text_to_table_keeps_the_record_on_the_last_piece_only() {
    use docxcore::editor::CellSep;
    let mut editor = editor_with(&format!(
        "<w:p><w:pPr><w:rPr><w:del w:id=\"1\"/></w:rPr></w:pPr><w:r><w:t>a</w:t></w:r>\
         <w:r><w:tab/></w:r><w:r><w:t>b</w:t></w:r></w:p>{}",
        para("after")
    ));
    editor.anchor = Some(Caret::at(vec![0], 0));
    editor.caret = Caret::at(vec![0], 3);
    editor.text_to_table(CellSep::Tab, None).unwrap();
    let Block::Table(table) = &editor.doc.body[0] else {
        panic!("table")
    };
    let records: Vec<usize> = table.rows[0]
        .cells
        .iter()
        .map(|c| match &c.blocks[0] {
            Block::Paragraph(p) => p.props.mark_revisions.len(),
            _ => 0,
        })
        .collect();
    assert_eq!(records, [0, 1]);
    assert_eq!(mark_record_count(&document_to_xml(&editor.doc)), 1);
}
