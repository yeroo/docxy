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
