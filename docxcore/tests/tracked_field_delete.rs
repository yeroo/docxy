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
    assert_eq!(x.matches("<w:del ").count(), 3, "{x}");
    let ids: std::collections::HashSet<&str> = x
        .split("<w:del w:id=\"")
        .skip(1)
        .map(|r| &r[..r.find('"').unwrap()])
        .collect();
    assert_eq!(ids.len(), 3, "distinct ids: {x}");
    let field_del = x
        .split("<w:del ")
        .find(|d| d.contains("delInstrText"))
        .unwrap();
    let field_del = &field_del[..field_del.find("</w:del>").unwrap()];
    assert!(
        !field_del.contains(">b<") && !field_del.contains(">c<"),
        "{field_del}"
    );
    let mut accepted = Editor::new(ed.doc.clone());
    accepted.accept_all_revisions();
    assert_eq!(text(&accepted), "ad");
    ed.reject_all_revisions();
    assert_eq!(text(&ed), "ab7cd");
    assert_eq!(fields(&ed), 1);
}

#[test]
fn a_nested_simple_field_is_flattened_inside_the_deletion() {
    let nested = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
        <w:r><w:instrText> QUOTE </w:instrText></w:r>\
        <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
        <w:fldSimple w:instr=\" SEQ n \"><w:r><w:t>2</w:t></w:r></w:fldSimple>\
        <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
    let mut ed = editor(nested);
    assert_eq!(fields(&ed), 1, "the nested field collapses into the outer");
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(deletions(&ed), 1, "{}", xml(&ed));
    let x = xml(&ed);
    let del = &x[x.find("<w:del ").unwrap()..x.find("</w:del>").unwrap()];
    assert!(
        !del.contains("fldSimple") && !del.contains("hyperlink"),
        "{del}"
    );
    assert_eq!(del.matches("fldCharType=\"begin\"").count(), 2, "{del}");
}

#[test]
fn a_field_whose_result_holds_a_link_is_kept() {
    let linked = "<w:fldSimple w:instr=\" REF x \"><w:hyperlink w:anchor=\"x\">\
        <w:r><w:t>7</w:t></w:r></w:hyperlink></w:fldSimple>";
    let mut ed = editor(linked);
    assert_eq!(fields(&ed), 1, "a fldSimple loads as one field");
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(fields(&ed), 1, "{}", xml(&ed));
    assert!(!xml(&ed).contains("<w:del "), "{}", xml(&ed));
}

#[test]
fn a_locked_field_stays_locked_through_delete_and_reject() {
    let locked = "<w:fldSimple w:instr=\" PAGE \" w:fldLock=\"1\" w:dirty=\"true\">\
        <w:r><w:t>7</w:t></w:r></w:fldSimple>";
    let mut ed = editor(locked);
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(deletions(&ed), 1);
    assert!(xml(&ed).contains("w:fldLock=\"1\""), "{}", xml(&ed));
    ed.reject_all_revisions();
    let x = xml(&ed);
    assert!(
        x.contains("w:fldLock=\"1\"") && x.contains("w:dirty=\"true\""),
        "{x}"
    );
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

const LINKED: &str = "<w:fldSimple w:instr=\" REF x \"><w:hyperlink w:anchor=\"x\">\
    <w:r><w:t>7</w:t></w:r></w:hyperlink></w:fldSimple>";

fn press_backspace_twice(ed: &mut Editor) {
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    ed.backspace();
}

#[test]
fn a_nested_self_closing_field_with_a_greater_than_in_its_code() {
    let inner = "<w:fldSimple w:instr=\" IF a > b \"/>";
    let in_simple =
        format!("<w:fldSimple w:instr=\" QUOTE \"><w:r><w:t>x</w:t></w:r>{inner}</w:fldSimple>");
    let in_complex = format!(
        "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> QUOTE </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>x</w:t></w:r>{inner}\
         <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
    );
    for field in [in_simple, in_complex] {
        let mut ed = editor(&field);
        assert_eq!(fields(&ed), 1);
        press_backspace_twice(&mut ed);
        let x = xml(&ed);
        assert_eq!(deletions(&ed), 1, "{x}");
        let del = &x[x.find("<w:del ").unwrap()..x.find("</w:del>").unwrap()];
        assert!(!del.contains("fldSimple"), "{del}");
        assert!(del.contains("<w:delText>x</w:delText>"), "{del}");
        assert!(
            del.contains("IF a &gt; b") || del.contains("IF a > b"),
            "{del}"
        );
        assert_eq!(
            del.matches("fldCharType=\"begin\"").count(),
            del.matches("fldCharType=\"end\"").count(),
            "{del}"
        );
    }
}

#[test]
fn a_bookmark_in_a_simple_fields_result_is_recorded() {
    let field = "<w:fldSimple w:instr=\" REF x \"><w:bookmarkStart w:id=\"3\" w:name=\"bk\"/>\
        <w:r><w:t>7</w:t></w:r><w:bookmarkEnd w:id=\"3\"/></w:fldSimple>";
    let mut ed = editor(field);
    assert_eq!(fields(&ed), 1);
    press_backspace_twice(&mut ed);
    assert_eq!(deletions(&ed), 1, "{}", xml(&ed));
}

#[test]
fn a_kept_field_is_not_cut_and_pushes_no_undo_step() {
    let mut ed = editor(LINKED);
    assert_eq!(fields(&ed), 1);
    press_backspace_twice(&mut ed);
    assert_eq!(fields(&ed), 1);
    assert!(!ed.undo(), "the second Backspace pushed an empty step");

    // Ctrl+X on the field alone: nothing to cut, no step.
    let mut ed = editor(LINKED);
    ed.anchor = Some(Caret::at(vec![0], 2));
    ed.caret = Caret::at(vec![0], 3);
    assert!(ed.cut().is_none());
    assert_eq!(fields(&ed), 1);
    assert!(!ed.undo());

    // Over text, field, text: the text goes and the clip leaves the field
    // out, so a paste does not duplicate it.
    let mut ed = editor(LINKED);
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 4);
    let clip = ed.cut().expect("the text is cut");
    assert_eq!(fields(&ed), 1);
    ed.set_track_changes(None);
    ed.set_caret(Caret::at(vec![0], 0));
    ed.paste(&clip);
    assert_eq!(fields(&ed), 1, "{}", xml(&ed));

    // The plain-text cut leaves the field's result out of its text.
    let mut ed = editor(LINKED);
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 4);
    assert_eq!(ed.cut_text().as_deref(), Some("bc"));
    assert_eq!(fields(&ed), 1);
}
