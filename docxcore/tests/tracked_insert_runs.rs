//! A tracked insertion recorded on live runs (`RunProps::tracked_insert`, #624):
//! it saves as one `<w:ins>`, lists as one revision, and Accept / Reject act on
//! every run of it, while the text stays ordinary editable text for offsets.

use docxcore::editor::Editor;
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::markup::MarkupView;
use docxcore::model::{
    Block, Document, Inline, Paragraph, RevisionCategory, RevisionKind, RevisionMetadata, Run,
    RunProps, TrackedInsert,
};
use docxcore::serialize::document_to_xml;

fn meta(id: &str) -> RevisionMetadata {
    RevisionMetadata {
        id: Some(id.into()),
        author: Some("Ada".into()),
        date: Some("2026-01-02T03:04:05Z".into()),
        ..RevisionMetadata::default()
    }
}

fn recorded(text: &str, id: &str) -> Inline {
    let mut props = RunProps::default();
    props.tracked_insert = Some(TrackedInsert { metadata: meta(id) });
    // The underline cue a loaded insertion shows.
    props.underline = true;
    props.revision_cues.insertions = 1;
    props.revision_cues.underline_added = true;
    Inline::Run(Run {
        text: text.into(),
        props,
    })
}

fn plain(text: &str) -> Inline {
    Inline::Run(Run {
        text: text.into(),
        props: RunProps::default(),
    })
}

fn para(content: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph {
        props: Default::default(),
        content,
    })
}

/// `One ` + recorded `two ` + `three` in one paragraph, and recorded `four`
/// in a second.
fn doc() -> Document {
    let mut doc = Document {
        body: vec![
            para(vec![plain("One "), recorded("two ", "7"), plain("three")]),
            para(vec![recorded("four", "7")]),
        ],
    };
    doc.initialize_revision_targets();
    doc
}

#[test]
fn recorded_runs_save_as_one_ins_and_reload_as_a_revision() {
    let xml = document_to_xml(&doc());
    assert_eq!(
        xml.matches("<w:ins ").count(),
        2,
        "one per paragraph: {xml}"
    );
    assert!(
        xml.contains(
            "<w:ins w:id=\"7\" w:author=\"Ada\" w:date=\"2026-01-02T03:04:05Z\">\
             <w:r><w:t xml:space=\"preserve\">two </w:t></w:r></w:ins>"
        ),
        "{xml}"
    );
    // The underline is the display cue, not formatting: it is not written.
    assert!(!xml.contains("<w:u "), "{xml}");
    let back = parse_document_xml(&xml, &Relationships::default());
    let revisions = back.revisions();
    assert_eq!(revisions.len(), 2);
    assert!(
        revisions
            .iter()
            .all(|r| r.category == RevisionCategory::Inline(RevisionKind::Insert))
    );
    assert_eq!(
        back.plain_text().trim_end().replace('\n', "|"),
        "One two three|four"
    );
}

#[test]
fn adjacent_runs_of_one_insertion_are_one_ins_and_one_revision() {
    let mut doc = Document {
        body: vec![para(vec![
            recorded("ab", "3"),
            recorded("cd", "3"),
            plain("!"),
        ])],
    };
    doc.initialize_revision_targets();
    assert_eq!(doc.revisions().len(), 1);
    let xml = document_to_xml(&doc);
    assert_eq!(xml.matches("<w:ins ").count(), 1, "{xml}");
    assert_eq!(xml.matches("</w:ins>").count(), 1, "{xml}");
}

#[test]
fn recorded_text_is_editable_text_to_the_editor() {
    let mut ed = Editor::new(doc());
    // Offsets count the recorded text like any other.
    assert_eq!(
        ed.doc.plain_text().trim_end().replace('\n', "|"),
        "One two three|four"
    );
    ed.caret.path = vec![0];
    ed.caret.offset = 6; // inside "two "
    ed.insert_char('X');
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    assert_eq!(p.plain_text(), "One twXo three");
}

#[test]
fn accept_keeps_every_run_and_drops_the_record_and_cue() {
    let mut doc = doc();
    let target = doc.revisions()[0].target;
    assert!(doc.accept_revision(target).is_applied());
    assert!(doc.revisions().is_empty(), "both paragraphs' runs");
    let xml = document_to_xml(&doc);
    assert!(!xml.contains("<w:ins"), "{xml}");
    assert_eq!(
        doc.plain_text().trim_end().replace('\n', "|"),
        "One two three|four"
    );
    for block in &doc.body {
        let Block::Paragraph(p) = block else { continue };
        for inline in &p.content {
            let Inline::Run(r) = inline else { continue };
            assert!(!r.props.underline, "cue left on {:?}", r.text);
        }
    }
}

#[test]
fn reject_removes_every_run_of_the_insertion() {
    let mut doc = doc();
    let target = doc.revisions()[0].target;
    assert!(doc.reject_revision(target).is_applied());
    assert!(doc.revisions().is_empty());
    assert_eq!(doc.plain_text().trim_end().replace('\n', "|"), "One three");
}

#[test]
fn accept_all_and_reject_all_cover_recorded_runs() {
    let mut a = doc();
    let outcomes = a.accept_all_revisions();
    assert!(outcomes.iter().all(|o| o.is_applied()), "{outcomes:?}");
    assert!(a.revisions().is_empty());
    let mut r = doc();
    assert!(r.reject_all_revisions().iter().all(|o| o.is_applied()));
    assert_eq!(r.plain_text().trim_end().replace('\n', "|"), "One three");
}

#[test]
fn display_modes_show_recorded_text_as_they_do_loaded_text() {
    let doc = doc();
    let text = |v| {
        doc.markup_view(v)
            .plain_text()
            .trim_end()
            .replace('\n', "|")
    };
    assert_eq!(text(MarkupView::All), "One two three|four");
    assert_eq!(text(MarkupView::Simple), "One two three|four");
    assert_eq!(text(MarkupView::NoMarkup), "One two three|four");
    assert_eq!(text(MarkupView::Original), "One three");
    // Simple Markup unmarks the insertion.
    let simple = doc.markup_view(MarkupView::Simple);
    let Block::Paragraph(p) = &simple.body[0] else {
        panic!("paragraph")
    };
    assert!(
        p.content
            .iter()
            .all(|i| !matches!(i, Inline::Run(r) if r.props.underline))
    );
}
