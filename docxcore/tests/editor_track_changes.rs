//! Track Changes recording through the editor (#624): typing and deleting
//! become `w:ins` / `w:del`, undo is one step, Accept/Reject act on what was
//! recorded, and a save reloads to the same revisions.

use docxcore::editor::{Caret, Clip, Editor, TrackAuthor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::markup::MarkupView;
use docxcore::model::{Block, Document, Inline, RevisionCategory, RevisionKind, RevisionTarget};
use docxcore::serialize::document_to_xml;

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";
const NOW: &str = "2026-03-04T05:06:07Z";

fn clock() -> String {
    NOW.to_string()
}

fn parse(inner: &str) -> Document {
    parse_document_xml(
        &format!("<w:document {W}><w:body>{inner}</w:body></w:document>"),
        &Relationships::default(),
    )
}

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

/// An editor on `One two three.` with tracking on as Ada.
fn editor() -> Editor {
    let mut ed = Editor::new(parse(&para("One two three.")));
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock,
    }));
    ed
}

fn at(ed: &mut Editor, offset: usize) {
    ed.caret = Caret {
        path: vec![0],
        offset,
    };
    ed.anchor = None;
}

fn select(ed: &mut Editor, from: usize, to: usize) {
    ed.anchor = Some(Caret {
        path: vec![0],
        offset: from,
    });
    ed.caret = Caret {
        path: vec![0],
        offset: to,
    };
}

/// What the document reads as once every change is accepted: deleted text is
/// not in it, recorded insertions are.
fn text(ed: &Editor) -> String {
    ed.doc
        .markup_view(MarkupView::NoMarkup)
        .plain_text()
        .trim_end()
        .to_string()
}

fn xml(ed: &Editor) -> String {
    document_to_xml(&ed.doc)
}

fn kinds(doc: &Document) -> Vec<RevisionKind> {
    doc.revisions()
        .into_iter()
        .filter_map(|r| match r.category {
            RevisionCategory::Inline(k) => Some(k),
            _ => None,
        })
        .collect()
}

#[test]
fn typing_saves_as_one_ins_with_author_date_and_id() {
    let mut ed = editor();
    at(&mut ed, 8); // before "three"
    ed.insert_str("and a half ");
    assert_eq!(text(&ed), "One two and a half three.");
    let xml = xml(&ed);
    assert_eq!(xml.matches("<w:ins ").count(), 1, "{xml}");
    assert!(
        xml.contains(&format!(
            "<w:ins w:id=\"1\" w:author=\"Ada\" w:date=\"{NOW}\"><w:r><w:t xml:space=\"preserve\">and a half </w:t></w:r></w:ins>"
        )),
        "{xml}"
    );
    // The underline is the cue, not formatting.
    assert!(!xml.contains("<w:u "), "{xml}");
    // It reloads as the revision it was recorded as.
    let back = parse(
        &xml.split("<w:body>")
            .nth(1)
            .unwrap()
            .replace("</w:body></w:document>", ""),
    );
    assert_eq!(kinds(&back), [RevisionKind::Insert]);
    assert_eq!(back.plain_text().trim_end(), "One two and a half three.");
}

#[test]
fn deleting_saves_as_del_with_deltext_and_the_text_stays_in_the_file() {
    let mut ed = editor();
    select(&mut ed, 4, 8); // "two "
    ed.delete_selection();
    assert_eq!(text(&ed), "One three.");
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    assert_eq!(
        docxcore::editor::para_text_len(p),
        "One three.".chars().count(),
        "deleted text takes no offsets"
    );
    let xml = xml(&ed);
    assert!(
        xml.contains(&format!(
            "<w:del w:id=\"1\" w:author=\"Ada\" w:date=\"{NOW}\"><w:r><w:delText xml:space=\"preserve\">two </w:delText></w:r></w:del>"
        )),
        "{xml}"
    );
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete]);
    assert_eq!(ed.caret.offset, 4);
}

#[test]
fn consecutive_backspaces_and_deletes_join_into_one_deletion() {
    let mut ed = editor();
    at(&mut ed, 7); // after "One two"
    for _ in 0..3 {
        ed.backspace(); // "two" backwards
    }
    assert_eq!(text(&ed), "One  three.");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete]);
    assert!(xml(&ed).contains("<w:delText xml:space=\"preserve\">two</w:delText>"));
    // Forward delete after it: " three" -> the next chars join the same one.
    for _ in 0..2 {
        ed.delete_forward();
    }
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete]);
    assert!(xml(&ed).contains(">two t</w:delText>"), "{}", xml(&ed));
}

#[test]
fn deleting_inside_the_authors_own_insertion_removes_it_outright() {
    let mut ed = editor();
    at(&mut ed, 4);
    ed.insert_str("XY");
    ed.backspace();
    assert_eq!(text(&ed), "One Xtwo three.");
    assert_eq!(
        kinds(&ed.doc),
        [RevisionKind::Insert],
        "no w:del for own text"
    );
    assert!(!xml(&ed).contains("<w:del"), "{}", xml(&ed));
    ed.backspace();
    assert_eq!(text(&ed), "One two three.");
    assert!(kinds(&ed.doc).is_empty(), "an emptied insertion is gone");
}

#[test]
fn undo_of_a_recorded_edit_is_one_step_to_the_exact_prior_document() {
    let mut ed = editor();
    let before = ed.doc.clone();
    at(&mut ed, 8);
    ed.insert_str("and a half ");
    assert!(ed.undo());
    assert_eq!(ed.doc, before);
    assert!(!ed.undo(), "typing was a single step");
    select(&mut ed, 4, 8);
    ed.delete_selection();
    assert!(ed.undo());
    assert_eq!(ed.doc, before);
    // Redo brings the deletion back as recorded.
    assert!(ed.redo());
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete]);
}

#[test]
fn accept_and_reject_act_on_recorded_changes() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("new ");
    select(&mut ed, 4, 8); // "two "
    ed.delete_selection();
    assert_eq!(text(&ed), "One new three.");
    let revisions = ed.doc.revisions();
    assert_eq!(revisions.len(), 2);

    let mut accepted = ed.doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(accepted.plain_text().trim_end(), "One new three.");
    assert!(accepted.revisions().is_empty());

    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(rejected.plain_text().trim_end(), "One two three.");
    assert!(rejected.revisions().is_empty());

    // One at a time, through the editor, as one undo step each.
    let delete = revisions
        .iter()
        .find(|r| r.category == RevisionCategory::Inline(RevisionKind::Delete))
        .unwrap()
        .target;
    assert!(ed.reject_revision(delete).is_applied());
    assert_eq!(text(&ed), "One two new three.");
}

#[test]
fn tracking_off_after_a_recorded_insertion_does_not_extend_it() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("ab");
    ed.set_track_changes(None);
    ed.insert_str("cd"); // typed right after the insertion, untracked
    assert_eq!(text(&ed), "One two abcdthree.");
    let xml = xml(&ed);
    assert_eq!(xml.matches("<w:ins ").count(), 1, "{xml}");
    assert!(xml.contains(">ab</w:t></w:r></w:ins>"), "{xml}");
    assert!(xml.contains(">cd"), "{xml}");
    // And a deletion with tracking off removes outright.
    select(&mut ed, 4, 8);
    ed.delete_selection();
    assert!(!xml_has_del(&ed));
}

fn xml_has_del(ed: &Editor) -> bool {
    xml(ed).contains("<w:del ")
}

#[test]
fn paste_records_an_insertion_per_paragraph_and_a_copy_of_it_is_plain() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.paste(&Clip::from_text("a\nb"));
    let ids: Vec<_> = ed
        .doc
        .revisions()
        .into_iter()
        .filter(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
        .map(|r| r.metadata.id)
        .collect();
    assert_eq!(ids.len(), 2, "one insertion per paragraph");
    assert_ne!(ids[0], ids[1], "each with its own w:id");
    let xml = xml(&ed);
    assert_eq!(xml.matches("<w:ins ").count(), 2, "{xml}");
    // Copy the recorded text: the clip carries no record.
    select(&mut ed, 8, 9);
    let clip = ed.copy().expect("a selection");
    for inline in clip.paras.iter().flatten() {
        if let Inline::Run(r) = inline {
            assert!(r.props.tracked_insert.is_none());
            assert!(!r.props.underline);
        }
    }
    ed.set_track_changes(None);
    at(&mut ed, 0);
    ed.paste(&clip);
    assert_eq!(
        kinds(&ed.doc),
        [RevisionKind::Insert, RevisionKind::Insert],
        "pasting with tracking off adds no record"
    );
}

#[test]
fn replacing_records_a_deletion_and_an_insertion() {
    let mut ed = editor();
    select(&mut ed, 4, 7); // "two"
    ed.replace_current_with("2");
    assert_eq!(text(&ed), "One 2 three.");
    let mut k = kinds(&ed.doc);
    k.sort_by_key(|k| *k as u8);
    assert_eq!(k.len(), 2);
    assert!(k.contains(&RevisionKind::Insert) && k.contains(&RevisionKind::Delete));
    let xml = xml(&ed);
    assert!(
        xml.contains(">two</w:delText>") && xml.contains(">2</w:t></w:r></w:ins>"),
        "{xml}"
    );
}

#[test]
fn a_selection_across_paragraphs_records_its_text_and_keeps_the_marks() {
    let mut ed = Editor::new(parse(&format!(
        "{}{}{}",
        para("alpha"),
        para("beta"),
        para("gamma")
    )));
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock,
    }));
    ed.anchor = Some(Caret {
        path: vec![0],
        offset: 3,
    });
    ed.caret = Caret {
        path: vec![2],
        offset: 2,
    };
    ed.delete_selection();
    let blocks = ed
        .doc
        .body
        .iter()
        .filter(|b| matches!(b, Block::Paragraph(_)))
        .count();
    assert_eq!(blocks, 3, "paragraph marks stay");
    assert_eq!(text(&ed).replace('\n', "|"), "alp||mma");
    assert_eq!(kinds(&ed.doc).len(), 3, "one deletion per paragraph");
}

#[test]
fn revision_ids_clear_existing_revisions_and_comment_markers() {
    let doc = parse(
        "<w:p><w:r><w:t>ab</w:t></w:r>\
         <w:ins w:id=\"5\" w:author=\"Bob\"><w:r><w:t>X</w:t></w:r></w:ins>\
         <w:commentRangeStart w:id=\"9\"/><w:r><w:t>cd</w:t></w:r><w:commentRangeEnd w:id=\"9\"/>\
         <w:del w:id=\"odd\" w:author=\"Bob\"><w:r><w:delText>Z</w:delText></w:r></w:del></w:p>",
    );
    let mut ed = Editor::new(doc);
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock,
    }));
    at(&mut ed, 0);
    ed.insert_char('Q');
    let xml = xml(&ed);
    assert!(
        xml.contains("<w:ins w:id=\"10\" w:author=\"Ada\""),
        "above the existing 5 and the comment's 9, a non-numeric id ignored: {xml}"
    );
}

#[test]
fn a_fresh_target_is_assigned_to_every_recorded_revision() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("ab");
    select(&mut ed, 0, 3);
    ed.delete_selection();
    let targets: Vec<RevisionTarget> = ed.doc.revisions().iter().map(|r| r.target).collect();
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|t| t.is_assigned()));
    assert_ne!(targets[0], targets[1]);
}

#[test]
fn enter_in_the_middle_of_a_recorded_insertion_gives_each_half_its_own_id() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcd");
    ed.caret.offset = 10; // between "ab" and "cd"
    ed.insert_newline();
    let ids: Vec<_> = ed
        .doc
        .revisions()
        .into_iter()
        .filter(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
        .map(|r| r.metadata.id)
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    let xml = xml(&ed);
    let ins_ids: Vec<&str> = xml
        .split("<w:ins w:id=\"")
        .skip(1)
        .filter_map(|r| r.split('"').next())
        .collect();
    assert_eq!(ins_ids.len(), 2, "{xml}");
    assert_ne!(ins_ids[0], ins_ids[1], "{xml}");
}

fn insert_ids(xml: &str) -> Vec<String> {
    xml.split("<w:ins w:id=\"")
        .skip(1)
        .filter_map(|r| r.split('"').next().map(String::from))
        .collect()
}

fn assert_distinct_ins(ed: &Editor, what: &str, n: usize) {
    let ids = insert_ids(&xml(ed));
    assert_eq!(ids.len(), n, "{what}: {}", xml(ed));
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), n, "{what}: {}", xml(ed));
    let inserts = kinds(&ed.doc)
        .into_iter()
        .filter(|k| *k == RevisionKind::Insert)
        .count();
    assert_eq!(inserts, n, "{what}: the editor lists what a reload will");
}

#[test]
fn a_table_or_section_break_in_the_middle_of_an_insertion_unshares_its_id() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcd");
    ed.caret.offset = 10;
    ed.insert_table(1, 1, docxcore::table::AutoFit::Default)
        .unwrap();
    assert_distinct_ins(&ed, "table", 2);

    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcd");
    ed.caret.offset = 10;
    ed.insert_section_break(docxcore::sect::SectionStart::Continuous)
        .unwrap();
    assert_distinct_ins(&ed, "section break", 2);
}

#[test]
fn text_that_comes_between_two_runs_of_an_insertion_splits_its_id() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcd");
    // Untracked typing in the middle of it: the insertion is now two
    // stretches with something between.
    ed.set_track_changes(None);
    ed.caret.offset = 10;
    ed.insert_char('X');
    // The ids are settled right away, not at some later split.
    assert_distinct_ins(&ed, "right after the typing", 2);
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock,
    }));
    ed.insert_newline();
    ed.undo();
    // A later recorded edit settles the ids.
    at(&mut ed, 0);
    ed.paste(&Clip::from_text("z"));
    assert_distinct_ins(&ed, "untracked text between", 3);
}

fn as_author(ed: &mut Editor, name: &str) {
    ed.set_track_changes(Some(TrackAuthor {
        author: name.into(),
        clock,
    }));
}

/// Deleting inside another reviewer's recorded insertion keeps the text in
/// that insertion: Reject All and Original still drop it.
#[test]
fn deleting_in_another_reviewers_insertion_stays_inside_it() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abc");
    as_author(&mut ed, "Bob");
    at(&mut ed, 9);
    ed.delete_forward(); // the b
    assert_eq!(text(&ed), "One two acthree.");
    let xml = xml(&ed);
    assert!(
        xml.contains("<w:ins w:id=\"1\" w:author=\"Ada\"")
            && xml.contains("<w:del w:id=\"2\" w:author=\"Bob\""),
        "{xml}"
    );
    let ins = xml.find("<w:ins w:id=\"1\"").unwrap();
    let del = xml.find("<w:del w:id=\"2\"").unwrap();
    assert!(ins < del, "the deletion is nested in an insertion: {xml}");
    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(
        rejected.plain_text().trim_end(),
        "One two three.",
        "no b, no abc"
    );
    let original = ed.doc.markup_view(MarkupView::Original);
    assert_eq!(original.plain_text().trim_end(), "One two three.");
    let mut accepted = ed.doc.clone();
    accepted.accept_all_revisions();
    assert_eq!(accepted.plain_text().trim_end(), "One two acthree.");
    // Rejecting just the insertion leaves no empty deletion behind.
    let target = ed
        .doc
        .revisions()
        .into_iter()
        .find(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
        .unwrap()
        .target;
    assert!(ed.reject_revision(target).is_applied());
    assert_eq!(text(&ed), "One two three.");
    assert!(!xml_has_del(&ed), "{}", self::xml(&ed));
}

/// A recorded insertion split by Enter after its runs were formatted gives
/// every run of the second half the new revision, not the old target.
#[test]
fn rejecting_the_first_half_of_a_split_formatted_insertion_keeps_the_second() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcd");
    select(&mut ed, 9, 11); // "bc"
    ed.toggle_bold();
    ed.anchor = None;
    ed.caret.offset = 10; // between b and c
    ed.insert_newline();
    let first = ed
        .doc
        .revisions()
        .into_iter()
        .find(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
        .unwrap()
        .target;
    assert!(ed.reject_revision(first).is_applied());
    let rest = ed
        .doc
        .markup_view(MarkupView::NoMarkup)
        .plain_text()
        .replace('\n', "|");
    assert!(rest.contains("|cdthree."), "the second half stays: {rest}");
}

/// Ctrl+U on a recorded insertion underlines it for real: the display cue is
/// not the user's underline.
#[test]
fn underline_on_a_recorded_insertion_is_the_users_not_the_cue() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("xyz");
    assert!(!xml(&ed).contains("<w:u "));
    select(&mut ed, 8, 11);
    ed.toggle_underline();
    assert!(xml(&ed).contains("<w:u w:val=\"single\"/>"), "{}", xml(&ed));
    ed.toggle_underline();
    assert!(!xml(&ed).contains("<w:u "), "{}", xml(&ed));
    // Still drawn as an insertion.
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    assert!(
        p.content
            .iter()
            .any(|i| matches!(i, Inline::Run(r) if r.text == "xyz" && r.props.underline))
    );
}

#[test]
fn a_tracked_selection_across_a_table_records_its_text_and_keeps_the_table() {
    let doc = parse(&format!(
        "{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>{}",
        para("alpha"),
        para("cell"),
        para("gamma")
    ));
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    ed.anchor = Some(Caret {
        path: vec![0],
        offset: 3,
    });
    ed.caret = Caret {
        path: vec![2],
        offset: 2,
    };
    ed.delete_selection();
    assert!(
        ed.doc.body.iter().any(|b| matches!(b, Block::Table(_))),
        "the table stays"
    );
    assert_eq!(
        kinds(&ed.doc).len(),
        3,
        "alpha's tail, the cell, gamma's head"
    );
    assert!(xml(&ed).contains(">cell</w:delText>"), "{}", xml(&ed));
}

/// A text box anchored before the selection is not text between its ends.
#[test]
fn a_tracked_selection_leaves_text_boxes_alone() {
    let mut doc = Document {
        body: vec![
            Block::Paragraph(docxcore::model::Paragraph {
                props: Default::default(),
                content: vec![
                    Inline::TextBox {
                        raw: "<w:r><w:txbxContent></w:txbxContent></w:r>".into(),
                        blocks: vec![Block::Paragraph(docxcore::model::Paragraph {
                            props: Default::default(),
                            content: vec![Inline::Run(docxcore::model::Run {
                                text: "note".into(),
                                props: Default::default(),
                            })],
                        })],
                    },
                    Inline::Run(docxcore::model::Run {
                        text: "hello world".into(),
                        props: Default::default(),
                    }),
                ],
            }),
            Block::Paragraph(docxcore::model::Paragraph {
                props: Default::default(),
                content: vec![Inline::Run(docxcore::model::Run {
                    text: "tail".into(),
                    props: Default::default(),
                })],
            }),
        ],
    };
    doc.initialize_revision_targets();
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    ed.anchor = Some(Caret {
        path: vec![0],
        offset: 5,
    });
    ed.caret = Caret {
        path: vec![1],
        offset: 2,
    };
    ed.delete_selection();
    assert_eq!(
        kinds(&ed.doc).len(),
        2,
        "the tail of the first, the head of the second"
    );
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    let Some(Inline::TextBox { blocks, .. }) = p.content.first() else {
        panic!("the text box is still first")
    };
    assert_eq!(blocks[0].plain_text(), "note");
    assert!(!xml(&ed).contains(">note</w:delText>"));
}

/// An insertion with many deletions inside it is still one revision.
#[test]
fn many_deletions_inside_an_insertion_leave_it_one_revision() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abcdefghijklmnopqrstuvwxyz0123456789");
    as_author(&mut ed, "Bob");
    for k in 0..17 {
        at(&mut ed, 9 + k);
        ed.delete_forward();
    }
    let inserts = kinds(&ed.doc)
        .into_iter()
        .filter(|k| *k == RevisionKind::Insert)
        .count();
    assert_eq!(inserts, 1, "{}", xml(&ed));
}

/// The nested deletion is one `w:ins` around one `w:del`, and it survives a
/// save and reload: Reject All brings the original back.
#[test]
fn a_deletion_in_another_reviewers_insertion_saves_nested_and_reloads() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("abc");
    as_author(&mut ed, "Bob");
    at(&mut ed, 9);
    ed.delete_forward();
    let saved = xml(&ed);
    assert_eq!(saved.matches("<w:ins ").count(), 1, "{saved}");
    assert_eq!(saved.matches("<w:del ").count(), 1, "{saved}");
    let (ins, close) = (
        saved.find("<w:ins ").unwrap(),
        saved.find("</w:ins>").unwrap(),
    );
    let del = saved.find("<w:del ").unwrap();
    assert!(ins < del && del < close, "{saved}");
    let inner = saved
        .split("<w:body>")
        .nth(1)
        .unwrap()
        .replace("</w:body></w:document>", "");
    let mut back = parse(&inner);
    let inserts = kinds(&back)
        .into_iter()
        .filter(|k| *k == RevisionKind::Insert)
        .count();
    assert_eq!(inserts, 1);
    assert!(back.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(back.plain_text().trim_end(), "One two three.");
}

/// Format patches and the ribbon's pressed state see the user's underline,
/// not the insertion cue.
#[test]
fn format_and_pressed_state_ignore_the_insertion_cue() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.insert_str("xyz");
    ed.caret.offset = 9;
    assert!(
        !ed.caret_props().user_underline(),
        "the cue is not underline"
    );
    let patch = docxcore::agent::RunPatch {
        underline: Some(true),
        ..Default::default()
    };
    docxcore::agent::format_range(&mut ed, 0, 0, &patch).unwrap();
    assert!(xml(&ed).contains("<w:u w:val=\"single\"/>"), "{}", xml(&ed));
    let off = docxcore::agent::RunPatch {
        underline: Some(false),
        ..Default::default()
    };
    docxcore::agent::format_range(&mut ed, 0, 0, &off).unwrap();
    assert!(!xml(&ed).contains("<w:u "), "{}", xml(&ed));
}

/// Two paragraphs of one text box, selected across: both stay, the text is
/// recorded as deleted (Reject All brings it back), nothing is removed outright.
#[test]
fn a_tracked_selection_within_one_text_box_is_recorded() {
    let box_para = |t: &str| {
        Block::Paragraph(docxcore::model::Paragraph {
            props: Default::default(),
            content: vec![Inline::Run(docxcore::model::Run {
                text: t.into(),
                props: Default::default(),
            })],
        })
    };
    let mut doc = Document {
        body: vec![Block::Paragraph(docxcore::model::Paragraph {
            props: Default::default(),
            content: vec![Inline::TextBox {
                raw: "<w:r><w:txbxContent></w:txbxContent></w:r>".into(),
                blocks: vec![box_para("alpha"), box_para("beta")],
            }],
        })],
    };
    doc.initialize_revision_targets();
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    ed.anchor = Some(Caret {
        path: vec![0, 0, 0],
        offset: 3,
    });
    ed.caret = Caret {
        path: vec![0, 0, 1],
        offset: 2,
    };
    ed.delete_selection();
    let Block::Paragraph(host) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    let Some(Inline::TextBox { blocks, .. }) = host.content.first() else {
        panic!("text box")
    };
    assert_eq!(blocks.len(), 2, "both paragraphs stay");
    assert_eq!(
        kinds(&ed.doc).len(),
        2,
        "recorded, not removed: {}",
        xml(&ed)
    );
    let mut rejected = ed.doc.clone();
    rejected.reject_all_revisions();
    let Block::Paragraph(host) = &rejected.body[0] else {
        panic!("paragraph")
    };
    let Some(Inline::TextBox { blocks, .. }) = host.content.first() else {
        panic!("text box")
    };
    assert_eq!(blocks[0].plain_text(), "alpha");
    assert_eq!(blocks[1].plain_text(), "beta");
}

/// A text box nested in the box the selection is in is skipped like any
/// other: its text is not between the endpoints.
#[test]
fn a_tracked_selection_in_a_text_box_skips_a_box_nested_in_it() {
    let para = |content: Vec<Inline>| {
        Block::Paragraph(docxcore::model::Paragraph {
            props: Default::default(),
            content,
        })
    };
    let run = |t: &str| {
        Inline::Run(docxcore::model::Run {
            text: t.into(),
            props: Default::default(),
        })
    };
    let inner = Inline::TextBox {
        raw: "<w:r><w:txbxContent></w:txbxContent></w:r>".into(),
        blocks: vec![para(vec![run("inner")])],
    };
    let mut doc = Document {
        body: vec![para(vec![Inline::TextBox {
            raw: "<w:r><w:txbxContent></w:txbxContent></w:r>".into(),
            blocks: vec![para(vec![inner, run("first")]), para(vec![run("second")])],
        }])],
    };
    doc.initialize_revision_targets();
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    ed.anchor = Some(Caret {
        path: vec![0, 0, 0],
        offset: 2,
    });
    ed.caret = Caret {
        path: vec![0, 0, 1],
        offset: 3,
    };
    ed.delete_selection();
    assert_eq!(
        kinds(&ed.doc).len(),
        2,
        "first's tail and second's head: {}",
        xml(&ed)
    );
    assert!(!xml(&ed).contains(">inner</w:delText>"), "{}", xml(&ed));
}

/// A copy over a tracked deletion leaves the deleted text out, so the text it
/// puts on the clipboard (what Keep Text Only pastes, #753) never brings it
/// back as live text.
#[test]
fn a_copy_over_a_tracked_deletion_leaves_the_deleted_text_out_753() {
    let mut ed = editor();
    select(&mut ed, 4, 8); // "two "
    ed.delete_selection();
    ed.set_track_changes(None);
    select(&mut ed, 0, 10);
    let clip = ed.copy().expect("a selection");
    assert_eq!(clip.to_text(), "One three.");
}

/// Keep Text Only (#753) is recorded as a paste is: one tracked insertion,
/// and the text reads as pasted once accepted.
#[test]
fn paste_text_only_records_an_insertion_753() {
    let mut ed = editor();
    at(&mut ed, 4);
    ed.paste_text_only("new ");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Insert]);
    assert_eq!(text(&ed), "One new two three.");
    assert!(xml(&ed).contains("<w:ins "));
}

/// The issue's TOC entry: an anchor link holding `Intro`, a tab and `1`
/// (loaded as two links with the tab between them), then `After`.
const TOC_ENTRY: &str = "<w:p><w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>Intro</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>1</w:t></w:r></w:hyperlink></w:p><w:p><w:r><w:t>After</w:t></w:r></w:p>";

/// A paragraph with a simple link in the middle: `See here.`.
const SEE_HERE: &str = "<w:p><w:r><w:t xml:space=\"preserve\">See </w:t></w:r><w:hyperlink w:anchor=\"x\"><w:r><w:t>here</w:t></w:r></w:hyperlink><w:r><w:t>.</w:t></w:r></w:p>";

fn tracked(inner: &str) -> Editor {
    let mut ed = Editor::new(parse(inner));
    as_author(&mut ed, "Ada");
    ed
}

/// The paragraph at `index` of `doc`'s body.
fn para_at(doc: &Document, index: usize) -> &docxcore::model::Paragraph {
    let Block::Paragraph(p) = &doc.body[index] else {
        panic!("paragraph {index}")
    };
    p
}

/// Each top-level inline of `p`: a link as its anchor, live text and deleted
/// text (`link _Toc1 "Intro" del ""`), `tab`, a run's text, or a deletion's
/// text (`deleted "…"`).
fn shape(p: &docxcore::model::Paragraph) -> Vec<String> {
    fn live(content: &[Inline]) -> String {
        content
            .iter()
            .map(|i| match i {
                Inline::Run(r) => r.text.clone(),
                Inline::Tab(_) => "\t".into(),
                _ => String::new(),
            })
            .collect()
    }
    fn deleted(content: &[Inline]) -> String {
        content
            .iter()
            .map(|i| match i {
                Inline::Revision {
                    kind: RevisionKind::Delete,
                    content,
                    ..
                } => live(content),
                _ => String::new(),
            })
            .collect()
    }
    p.content
        .iter()
        .map(|i| match i {
            Inline::Hyperlink(h) => {
                let runs: String = h.runs.iter().map(|r| r.text.as_str()).collect();
                format!(
                    "link {} {:?} del {:?}",
                    h.anchor.as_deref().unwrap_or("-"),
                    runs + &live(&h.content),
                    deleted(&h.content)
                )
            }
            Inline::Tab(_) => "tab".into(),
            Inline::Run(r) => format!("{:?}", r.text),
            Inline::Revision {
                kind: RevisionKind::Delete,
                content,
                ..
            } => format!("deleted {:?}", live(content)),
            other => format!("{other:?}"),
        })
        .collect()
}

/// `xml` with every `w:id` value blanked, to match the rest literally.
fn without_ids(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(at) = rest.find("w:id=\"") {
        out.push_str(&rest[..at + 6]);
        rest = &rest[at + 6..];
        rest = &rest[rest.find('"').unwrap()..];
    }
    out.push_str(rest);
    out
}

fn reload(ed: &Editor) -> Document {
    let saved = xml(ed);
    let inner = saved
        .split("<w:body>")
        .nth(1)
        .unwrap()
        .replace("</w:body></w:document>", "");
    parse(&inner)
}

/// Select from the start of the TOC entry to `Af|ter`.
fn select_toc_into_after(ed: &mut Editor) {
    ed.anchor = Some(Caret {
        path: vec![0],
        offset: 0,
    });
    ed.caret = Caret {
        path: vec![1],
        offset: 2,
    };
}

/// The issue's repro: a tracked selection from a TOC entry's link into the
/// next paragraph records the link's text inside the link, each `w:del` on
/// its own side of a link boundary, and nothing is lost (#1462).
#[test]
fn a_tracked_selection_across_paragraphs_records_link_text_1462() {
    let mut ed = tracked(TOC_ENTRY);
    let before = ed.doc.clone();
    select_toc_into_after(&mut ed);
    ed.backspace();
    assert_eq!(
        shape(para_at(&ed.doc, 0)),
        ["link _Toc1 \"\" del \"Intro\\t1\""],
        "{}",
        xml(&ed)
    );
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete; 2]);
    let saved = without_ids(&xml(&ed));
    let del = format!("<w:del w:id=\"\" w:author=\"Ada\" w:date=\"{NOW}\">");
    // The entry stays one link, its tab recorded inside the same deletion (#920).
    let p0 = format!(
        "<w:hyperlink w:anchor=\"_Toc1\">{del}<w:r><w:delText xml:space=\"preserve\">Intro</w:delText></w:r>\
         <w:r><w:tab/></w:r><w:r><w:delText xml:space=\"preserve\">1</w:delText></w:r></w:del></w:hyperlink></w:p>"
    );
    assert!(saved.contains(&p0), "{saved}");
    assert!(
        saved.contains(&format!(
            "{del}<w:r><w:delText xml:space=\"preserve\">Af</w:delText></w:r></w:del>"
        )),
        "paragraph 1 keeps its deletion: {saved}"
    );

    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(
        shape(para_at(&rejected, 0)),
        ["link _Toc1 \"Intro\\t1\" del \"\""]
    );
    assert_eq!(para_at(&rejected, 1).plain_text(), "After");

    let mut accepted = ed.doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(para_at(&accepted, 0).plain_text(), "");
    assert_eq!(para_at(&accepted, 1).plain_text(), "ter");

    // Undo is one step back to the exact prior document.
    assert!(ed.undo());
    assert_eq!(ed.doc, before);
    assert!(!ed.undo());
}

/// The recorded link deletions survive a save and reload, Reject All there
/// restores the links' text, and that survives another save (#1462).
#[test]
fn a_tracked_link_deletion_saves_and_reloads_1462() {
    let mut ed = tracked(TOC_ENTRY);
    select_toc_into_after(&mut ed);
    ed.delete_selection();
    let mut back = reload(&ed);
    assert_eq!(kinds(&back), [RevisionKind::Delete; 2]);
    assert_eq!(shape(para_at(&back, 0)), shape(para_at(&ed.doc, 0)));
    assert!(back.reject_all_revisions().iter().all(|o| o.is_applied()));
    let rejected = Editor::new(back);
    let again = reload(&rejected);
    assert!(again.revisions().is_empty(), "{}", xml(&rejected));
    assert_eq!(
        shape(para_at(&again, 0)),
        ["link _Toc1 \"Intro\\t1\" del \"\""]
    );
    assert_eq!(para_at(&again, 1).plain_text(), "After");
}

/// Backspaces inside a simple link record each character inside it, joined
/// into one deletion; once all of it is recorded the link stays, zero-width
/// (#1462).
#[test]
fn backspace_in_a_simple_link_records_one_deletion_inside_it_1462() {
    let mut ed = tracked(SEE_HERE);
    at(&mut ed, 8); // after "here"
    ed.backspace();
    assert_eq!(text(&ed), "See her.");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete]);
    for _ in 0..3 {
        ed.backspace();
    }
    assert_eq!(text(&ed), "See .");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete], "{}", xml(&ed));
    assert_eq!(
        shape(para_at(&ed.doc, 0)),
        ["\"See \"", "link x \"\" del \"here\"", "\".\""]
    );
    assert_eq!(ed.caret.offset, 4);
    assert_eq!(docxcore::editor::para_text_len(para_at(&ed.doc, 0)), 5);
    assert!(
        xml(&ed).contains("<w:hyperlink w:anchor=\"x\"><w:del "),
        "{}",
        xml(&ed)
    );
    // Delete from there steps over the recorded link to the `.`, a deletion
    // of its own outside the link.
    ed.delete_forward();
    assert_eq!(
        shape(para_at(&ed.doc, 0)),
        ["\"See \"", "link x \"\" del \"here\"", "deleted \".\""]
    );
}

/// A selection over a link's edge records the text on each side, each in
/// its own `w:del`: the one inside the link never joins the one outside
/// (#1462).
#[test]
fn a_tracked_selection_over_a_link_edge_keeps_the_del_inside_the_link_1462() {
    let mut ed = tracked(SEE_HERE);
    select(&mut ed, 2, 6); // "e " outside the link, "he" inside
    ed.delete_selection();
    assert_eq!(text(&ed), "Sere.");
    assert_eq!(
        shape(para_at(&ed.doc, 0)),
        [
            "\"Se\"",
            "deleted \"e \"",
            "link x \"re\" del \"he\"",
            "\".\""
        ]
    );
    assert_eq!(kinds(&ed.doc), [RevisionKind::Delete; 2]);
    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(rejected.plain_text().trim_end(), "See here.");
    let Some(Inline::Hyperlink(_)) = para_at(&rejected, 0)
        .content
        .iter()
        .find(|i| matches!(i, Inline::Hyperlink(_)))
    else {
        panic!("the link stays")
    };
}

/// A link that loads with its raw XML kept (an external link holding a tab)
/// is rebuilt on save, so a deletion recorded inside it is written (#1462).
#[test]
fn a_tracked_delete_in_a_raw_kept_external_link_saves_inside_it_1462() {
    let rels = docxcore::load::parse_rels_xml(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/\" TargetMode=\"External\"/></Relationships>",
    );
    let doc = parse_document_xml(
        &format!(
            "<w:document {W} xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body><w:p><w:hyperlink r:id=\"rId9\"><w:r><w:t>Go</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>here</w:t></w:r></w:hyperlink></w:p></w:body></w:document>"
        ),
        &rels,
    );
    let Inline::Hyperlink(h) = &para_at(&doc, 0).content[0] else {
        panic!("link")
    };
    assert!(h.runs.is_empty() && h.raw.is_some() && !h.content_changed);
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    at(&mut ed, 0);
    ed.delete_forward();
    let saved = xml(&ed);
    let link = &saved[saved.find("<w:hyperlink").unwrap()..saved.find("</w:hyperlink>").unwrap()];
    assert!(link.contains("r:id=\"rId9\""), "{saved}");
    assert!(
        link.contains("<w:delText xml:space=\"preserve\">G</w:delText>"),
        "{saved}"
    );
    assert!(
        link.contains("<w:t xml:space=\"preserve\">o</w:t>"),
        "{saved}"
    );
}

fn para_mut_at(doc: &mut Document, index: usize) -> &mut docxcore::model::Paragraph {
    let Block::Paragraph(p) = &mut doc.body[index] else {
        panic!("paragraph {index}")
    };
    p
}

/// Whether the first paragraph holds no link.
fn no_link(doc: &Document) -> bool {
    !para_at(doc, 0)
        .content
        .iter()
        .any(|i| matches!(i, Inline::Hyperlink(_)))
}

fn ins_in_link(saved: &str) -> bool {
    let link = &saved[saved.find("<w:hyperlink").unwrap()..saved.find("</w:hyperlink>").unwrap()];
    link.contains("<w:ins ")
}

/// Typing in the middle of a simple link records the text inside it, as one
/// insertion; Reject All and Accept All act on it, also after a reload
/// (#1465).
#[test]
fn typing_in_a_simple_link_is_recorded_inside_it_1465() {
    let mut ed = tracked(SEE_HERE);
    at(&mut ed, 5); // "h|ere"
    ed.insert_str("XY");
    assert_eq!(text(&ed), "See hXYere.");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Insert]);
    let saved = xml(&ed);
    assert!(
        saved.contains("<w:hyperlink w:anchor=\"x\">") && ins_in_link(&saved),
        "{saved}"
    );
    let mut back = reload(&ed);
    assert_eq!(kinds(&back), [RevisionKind::Insert]);
    assert!(back.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(back.plain_text().trim_end(), "See here.");
    let mut accepted = reload(&ed);
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(accepted.plain_text().trim_end(), "See hXYere.");
    assert!(accepted.revisions().is_empty());
    // The same, on the live document.
    let mut live = ed.doc.clone();
    assert!(live.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(live.plain_text().trim_end(), "See here.");
}

/// A TOC entry's link (complex form, a tab inside) records typing too, and a
/// Reject All after a save and reload restores the entry (#1465).
#[test]
fn typing_in_a_toc_entry_link_is_recorded_and_rejects_after_reload_1465() {
    let mut ed = tracked(TOC_ENTRY);
    at(&mut ed, 2); // "In|tro"
    ed.insert_str("zz");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Insert]);
    assert!(ins_in_link(&xml(&ed)), "{}", xml(&ed));
    let mut back = reload(&ed);
    assert_eq!(kinds(&back), [RevisionKind::Insert]);
    assert!(back.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(para_at(&back, 0).plain_text(), "Intro\t1");
    let mut accepted = reload(&ed);
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(para_at(&accepted, 0).plain_text(), "Inzztro\t1");
}

/// Replace inside a link records the text put in, so Reject
/// All gives back the original, not both (#1465).
#[test]
fn replace_in_a_link_rejects_to_the_original_1465() {
    let mut ed = tracked(TOC_ENTRY);
    select(&mut ed, 2, 4); // "tr"
    ed.replace_current_with("XY");
    assert_eq!(text(&ed).lines().next().unwrap(), "InXYo\t1");
    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(para_at(&rejected, 0).plain_text(), "Intro\t1");
    let mut back = reload(&ed);
    assert!(back.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(para_at(&back, 0).plain_text(), "Intro\t1");
}

/// A Replace across a link's edge records the text on both sides.
#[test]
fn replace_across_a_link_edge_records_both_sides_1465() {
    let mut ed = tracked(SEE_HERE);
    select(&mut ed, 2, 6); // "e " outside, "he" inside
    ed.replace_current_with("QQ");
    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(rejected.plain_text().trim_end(), "See here.");
    let mut accepted = ed.doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(accepted.plain_text().trim_end(), "SeQQre.");
}

/// Typing that extends the author's own insertion in a link adds no revision.
#[test]
fn typing_on_in_a_link_extends_one_insertion_1465() {
    let mut ed = tracked(SEE_HERE);
    at(&mut ed, 5);
    ed.insert_str("a");
    ed.insert_str("b");
    ed.insert_str("c");
    assert_eq!(text(&ed), "See habcere.");
    assert_distinct_ins(&ed, "typing in a link", 1);
}

/// With tracking off, typing in a link stays plain, and in the middle of an
/// insertion in a link it interrupts it.
#[test]
fn untracked_typing_in_a_link_interrupts_an_insertion_1465() {
    let mut ed = tracked(SEE_HERE);
    at(&mut ed, 5);
    ed.insert_str("abcd");
    ed.set_track_changes(None);
    at(&mut ed, 7); // "ab|cd"
    ed.insert_str("--");
    assert_eq!(text(&ed), "See hab--cdere.");
    assert_distinct_ins(&ed, "interrupted", 2);
    let mut rejected = ed.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(rejected.plain_text().trim_end(), "See h--ere.");
    // Plain typing in a link without any insertion records nothing.
    let mut plain = Editor::new(parse(SEE_HERE));
    at(&mut plain, 5);
    plain.insert_str("zz");
    assert!(plain.doc.revisions().is_empty());
}

fn raw_kept_link_with_insert() -> Document {
    let rels = docxcore::load::parse_rels_xml(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/\" TargetMode=\"External\"/></Relationships>",
    );
    parse_document_xml(
        &format!(
            "<w:document {W} xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body><w:p><w:hyperlink r:id=\"rId9\"><w:r><w:t>Go</w:t></w:r><w:r><w:tab/></w:r><w:ins w:id=\"7\" w:author=\"Bo\" w:date=\"2024-01-01T00:00:00Z\"><w:r><w:t>new</w:t></w:r></w:ins></w:hyperlink><w:p/></w:p></w:body></w:document>"
        ),
        &rels,
    )
}

/// Accept or Reject All of an insertion inside a link that kept its raw XML
/// is written: the save does not fall back to the stale raw (#1465).
#[test]
fn accept_and_reject_in_a_raw_kept_link_survive_a_save_1465() {
    let doc = raw_kept_link_with_insert();
    let Inline::Hyperlink(h) = &para_at(&doc, 0).content[0] else {
        panic!("link")
    };
    assert!(h.raw.is_some() && !h.content_changed);
    let mut rejected = doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    let saved = document_to_xml(&rejected);
    assert!(
        !saved.contains("new") && saved.contains("r:id=\"rId9\""),
        "{saved}"
    );
    let mut accepted = doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    let saved = document_to_xml(&accepted);
    assert!(
        saved.contains("new") && !saved.contains("<w:ins "),
        "{saved}"
    );
}

/// A recorded insertion inside a link that kept its raw XML, with the link
/// not yet marked changed (as a document built in memory can be): Accept and
/// Reject All must mark it, or the save writes the stale raw (#1465).
#[test]
fn sweeping_a_recorded_insertion_marks_a_raw_kept_link_changed_1465() {
    let mut ed = tracked(TOC_ENTRY);
    at(&mut ed, 2);
    ed.insert_str("zz");
    let mut doc = ed.doc.clone();
    let Inline::Hyperlink(h) = &mut para_mut_at(&mut doc, 0).content[0] else {
        panic!("link")
    };
    h.raw =
        Some("<w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>STALE</w:t></w:r></w:hyperlink>".into());
    h.content_changed = false;
    let mut rejected = doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    let saved = document_to_xml(&rejected);
    assert!(!saved.contains("STALE") && !saved.contains("zz"), "{saved}");
    let mut accepted = doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    let saved = document_to_xml(&accepted);
    assert!(!saved.contains("STALE") && saved.contains("zz"), "{saved}");
}

/// Accept All sweeps through a link nothing was recorded in: it keeps its raw
/// XML verbatim and is not marked changed (#1465).
#[test]
fn a_sweep_leaves_an_untouched_raw_kept_link_alone_1465() {
    let rels = docxcore::load::parse_rels_xml(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" Target=\"https://example.com/\" TargetMode=\"External\"/></Relationships>",
    );
    let doc = parse_document_xml(
        &format!(
            "<w:document {W} xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body><w:p><w:hyperlink r:id=\"rId9\" w:history=\"1\"><w:r><w:t>Go</w:t></w:r><w:r><w:tab/></w:r></w:hyperlink></w:p><w:p><w:r><w:t>b</w:t></w:r></w:p></w:body></w:document>"
        ),
        &rels,
    );
    let before = document_to_xml(&doc);
    let mut ed = Editor::new(doc);
    as_author(&mut ed, "Ada");
    ed.caret = Caret {
        path: vec![1],
        offset: 1,
    };
    ed.insert_str("q");
    assert_eq!(kinds(&ed.doc), [RevisionKind::Insert]);
    let mut accepted = ed.doc.clone();
    assert!(
        accepted
            .accept_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    let Inline::Hyperlink(h) = &para_at(&accepted, 0).content[0] else {
        panic!("link")
    };
    assert!(h.raw.is_some() && !h.content_changed);
    let link =
        |x: &str| x[x.find("<w:hyperlink").unwrap()..x.find("</w:hyperlink>").unwrap()].to_string();
    assert_eq!(link(&document_to_xml(&accepted)), link(&before));
}

/// A copy of text typed inside a link is plain, and pasted it is plain
/// (tracking off) or a revision of its own (tracking on, written inside the
/// pasted link), and Reject All gives back the paragraph's own text (#1465).
#[test]
fn copying_text_recorded_in_a_link_and_pasting_it_1465() {
    let mut ed = tracked(SEE_HERE);
    at(&mut ed, 5);
    ed.insert_str("XY");
    select(&mut ed, 0, 8); // "See hXYe": the whole link run's start through the insertion
    let clip = ed.copy().expect("clip");
    // Copy is plain.
    let mut plain = Editor::new(parse(&para("zz")));
    at(&mut plain, 1);
    plain.paste(&clip);
    assert!(plain.doc.revisions().is_empty(), "{}", xml(&plain));
    // Paste with tracking on is a revision of its own.
    let mut tracked_paste = Editor::new(parse(&para("zz")));
    as_author(&mut tracked_paste, "Bo");
    at(&mut tracked_paste, 1);
    tracked_paste.paste(&clip);
    let kinds_now = kinds(&tracked_paste.doc);
    assert!(kinds_now.iter().all(|k| *k == RevisionKind::Insert) && !kinds_now.is_empty());
    let saved = xml(&tracked_paste);
    assert!(!saved.contains("w:author=\"Ada\""), "{saved}");
    assert!(ins_in_link(&saved), "{saved}");
    let mut rejected = tracked_paste.doc.clone();
    assert!(
        rejected
            .reject_all_revisions()
            .iter()
            .all(|o| o.is_applied())
    );
    assert_eq!(rejected.plain_text().trim_end(), "zz");
    assert!(no_link(&rejected));
    // Pasting back into the source document: Reject of the pasted text only.
    let mut again = Editor::new(ed.doc.clone());
    as_author(&mut again, "Bo");
    at(&mut again, 0);
    again.paste(&clip);
    let ids = insert_ids(&xml(&again));
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "{}", xml(&again));
    assert!(ids.len() >= 2, "{}", xml(&again));
}

/// Text pasted into a tracked document as a multi-run link is one revision,
/// listed once and rejected by its one target; the pasted text is in the
/// link's own `w:ins` (#1465).
#[test]
fn a_pasted_multi_run_link_is_one_revision_1465() {
    let mut src = tracked(SEE_HERE);
    at(&mut src, 5);
    src.insert_str("XY");
    select(&mut src, 4, 7); // "hXY": a plain run and a recorded one, in the link
    let clip = src.copy().expect("clip");
    let mut ed = Editor::new(parse(&para("zz")));
    as_author(&mut ed, "Bo");
    at(&mut ed, 1);
    ed.paste(&clip);
    assert_eq!(text(&ed), "zhXYz");
    assert!(ins_in_link(&xml(&ed)), "{}", xml(&ed));
    let revisions = ed.doc.revisions();
    assert_eq!(revisions.len(), 1, "{}", xml(&ed));
    assert!(ed.reject_revision(revisions[0].target).is_applied());
    assert_eq!(text(&ed), "zz");
    assert!(ed.doc.revisions().is_empty());
    assert!(no_link(&ed.doc), "{}", xml(&ed));
    // Typing where the link was lands as plain text, not in an empty link.
    ed.set_track_changes(None);
    at(&mut ed, 1);
    ed.insert_str("q");
    assert!(no_link(&ed.doc), "{}", xml(&ed));
    assert!(!xml(&ed).contains("<w:hyperlink"), "{}", xml(&ed));
}

/// Rejecting the only insertion of a loaded `w:hyperlink` takes the emptied
/// link with it, by Reject All and by its one target; Accept keeps the link
/// (#1465).
#[test]
fn rejecting_a_links_only_insertion_drops_the_link_1465() {
    let inner = "<w:p><w:hyperlink w:anchor=\"x\"><w:ins w:id=\"7\" w:author=\"Bo\" w:date=\"2024-01-01T00:00:00Z\"><w:r><w:t>new</w:t></w:r></w:ins></w:hyperlink></w:p>";
    let mut all = parse(inner);
    assert!(all.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert!(no_link(&all));
    let mut ed = Editor::new(parse(inner));
    let target = ed.doc.revisions()[0].target;
    assert!(ed.reject_revision(target).is_applied());
    assert!(no_link(&ed.doc), "{}", xml(&ed));
    at(&mut ed, 0);
    ed.insert_str("q");
    assert!(!xml(&ed).contains("<w:hyperlink"), "{}", xml(&ed));
    assert_eq!(text(&ed), "q");
    let mut kept = Editor::new(parse(inner));
    let target = kept.doc.revisions()[0].target;
    assert!(kept.accept_revision(target).is_applied());
    assert!(!no_link(&kept.doc), "{}", xml(&kept));
    assert_eq!(text(&kept), "new");
}
