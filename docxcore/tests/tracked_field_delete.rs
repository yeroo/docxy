//! #1470: a tracked delete of a field (one editor offset, #642) is recorded as
//! a `w:del` of its own that Reject restores and Accept drops, not removed
//! untracked.

use docxcore::editor::{Caret, Editor, TrackAuthor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, RevisionKind};
use docxcore::serialize::document_to_xml;

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

fn clock() -> String {
    "2026-03-04T05:06:07Z".to_string()
}

fn parse(inner: &str) -> Document {
    parse_document_xml(
        &format!("<w:document {W}><w:body>{inner}</w:body></w:document>"),
        &Relationships::default(),
    )
}

const SIMPLE: &str = "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>7</w:t></w:r></w:fldSimple>";
const COMPLEX: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
    <w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
    <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>7</w:t></w:r>\
    <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
const SYM: &str = "<w:r><w:sym w:font=\"Wingdings\" w:char=\"F0FC\"/></w:r>";

/// An editor on `ab<field>cd`, tracking as Ada.
fn editor(field: &str) -> Editor {
    let mut ed = Editor::new(parse(&format!(
        "<w:p><w:r><w:t>ab</w:t></w:r>{field}<w:r><w:t>cd</w:t></w:r></w:p>"
    )));
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock,
    }));
    ed
}

fn xml(ed: &Editor) -> String {
    document_to_xml(&ed.doc)
}

fn fields(ed: &Editor) -> usize {
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    p.content
        .iter()
        .filter(|i| matches!(i, Inline::Field { .. }))
        .count()
}

fn deletions(ed: &Editor) -> usize {
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    p.content
        .iter()
        .filter(|i| {
            matches!(i, Inline::Revision { kind: RevisionKind::Delete, content, .. }
                if content.iter().any(|c| matches!(c, Inline::Field { .. })))
        })
        .count()
}

fn text(ed: &Editor) -> String {
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    p.plain_text()
}

#[test]
fn tracked_backspace_and_delete_record_a_field_deletion() {
    for field in [SIMPLE, COMPLEX] {
        for (at, forward) in [(3, false), (2, true)] {
            let mut ed = editor(field);
            ed.set_caret(Caret::at(vec![0], at));
            if forward {
                ed.delete_forward();
            } else {
                ed.backspace();
            }
            if ed.has_selection() {
                // A second press deletes what the first selected.
                if forward {
                    ed.delete_forward();
                } else {
                    ed.backspace();
                }
            }
            assert_eq!(fields(&ed), 0, "the field left the live content");
            assert_eq!(deletions(&ed), 1, "{}", xml(&ed));
            let x = xml(&ed);
            assert!(x.contains("<w:del "), "{x}");
            assert!(x.contains("w:delInstrText"), "{x}");
            assert!(x.contains("<w:delText"), "{x}");
            let del = &x[x.find("<w:del ").unwrap()..x.find("</w:del>").unwrap()];
            assert!(!del.contains("fldSimple"), "{del}");
            assert!(ed.undo());
            assert_eq!(fields(&ed), 1);
            assert_eq!(deletions(&ed), 0);
        }
    }
}

#[test]
fn a_symbol_field_is_recorded_too() {
    let mut ed = editor(SYM);
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(deletions(&ed), 1, "{}", xml(&ed));
    assert!(xml(&ed).contains("<w:del "));
}

#[test]
fn reject_restores_the_field_and_accept_drops_it() {
    for field in [SIMPLE, COMPLEX] {
        let mut ed = editor(field);
        ed.set_caret(Caret::at(vec![0], 3));
        ed.backspace();
        if ed.has_selection() {
            ed.backspace();
        }
        let mut rejected = Editor::new(ed.doc.clone());
        rejected.reject_all_revisions();
        assert_eq!(fields(&rejected), 1, "{}", xml(&rejected));
        assert_eq!(deletions(&rejected), 0);
        let x = xml(&rejected);
        assert!(
            x.contains("instrText") && !x.contains("delInstrText"),
            "{x}"
        );
        assert!(!x.contains("<w:del "), "{x}");
        assert_eq!(text(&rejected), "ab7cd");

        let mut accepted = Editor::new(ed.doc.clone());
        accepted.accept_all_revisions();
        assert_eq!(fields(&accepted), 0);
        assert_eq!(text(&accepted), "abcd");
        assert!(!xml(&accepted).contains("instrText"));
    }
}

#[test]
fn the_deletion_survives_a_save_and_reload() {
    for field in [SIMPLE, COMPLEX] {
        let mut ed = editor(field);
        ed.set_caret(Caret::at(vec![0], 3));
        ed.backspace();
        if ed.has_selection() {
            ed.backspace();
        }
        let saved = xml(&ed);
        let body = &saved[saved.find("<w:body>").unwrap() + 8..saved.find("</w:body>").unwrap()];
        let mut again = Editor::new(parse(body));
        // The loader keeps a `w:del` holding field runs as loose runs (it
        // collapses no field inside a tracked change), so this pins the XML.
        assert_eq!(xml(&again), saved);
        assert_eq!(again.doc.revisions().len(), 1);
        again.reject_all_revisions();
        let x = xml(&again);
        assert!(
            x.contains("<w:instrText") && !x.contains("delInstrText"),
            "{x}"
        );
        assert!(
            x.contains("fldCharType=\"end\"") && !x.contains("<w:del "),
            "{x}"
        );
    }
}

#[test]
fn a_selection_over_text_field_text_records_all_three() {
    let mut ed = editor(SIMPLE);
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 4);
    assert!(ed.delete_selection());
    let x = xml(&ed);
    assert_eq!(deletions(&ed), 1, "{x}");
    assert!(x.matches("<w:del ").count() >= 2, "{x}");
    let mut accepted = Editor::new(ed.doc.clone());
    accepted.accept_all_revisions();
    assert_eq!(text(&accepted), "ad");
    ed.reject_all_revisions();
    assert_eq!(text(&ed), "ab7cd");
    assert_eq!(fields(&ed), 1);
}

#[test]
fn an_untracked_delete_still_removes_the_field() {
    let mut ed = editor(SIMPLE);
    ed.set_track_changes(None);
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(fields(&ed), 0);
    assert_eq!(deletions(&ed), 0);
    assert!(!xml(&ed).contains("<w:del "));
}
