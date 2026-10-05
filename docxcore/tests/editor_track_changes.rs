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
fn paste_records_one_insertion_and_a_copy_of_it_is_plain() {
    let mut ed = editor();
    at(&mut ed, 8);
    ed.paste(&Clip::from_text("a\nb"));
    let ids: Vec<_> = ed
        .doc
        .revisions()
        .into_iter()
        .filter(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
        .collect();
    assert_eq!(ids.len(), 1, "one insertion across both paragraphs");
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
        [RevisionKind::Insert],
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
