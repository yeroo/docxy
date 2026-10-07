//! Formatting toggled at an insertion point (#854): Ctrl+B with nothing
//! selected switches bold on for what is typed next, as an undo step of its
//! own, and moving the caret or any other edit leaves it behind.

use docxcore::editor::{Caret, Editor, TrackAuthor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, RunProps};
use docxcore::serialize::document_to_xml;

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

fn parse(inner: &str) -> Document {
    parse_document_xml(
        &format!("<w:document {W}><w:body>{inner}</w:body></w:document>"),
        &Relationships::default(),
    )
}

/// An editor on one empty paragraph.
fn empty() -> Editor {
    Editor::new(parse("<w:p/>"))
}

/// An editor on `text`, the caret at its end.
fn on(text: &str) -> Editor {
    let mut ed = Editor::new(parse(&format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>"
    )));
    ed.caret = Caret::at(vec![0], text.chars().count());
    ed
}

/// The first paragraph's inlines.
fn content(ed: &Editor) -> &[Inline] {
    match &ed.doc.body[0] {
        Block::Paragraph(p) => &p.content,
        other => panic!("not a paragraph: {other:?}"),
    }
}

/// The first paragraph's runs, as (text, bold).
fn runs(ed: &Editor) -> Vec<(String, bool)> {
    content(ed)
        .iter()
        .filter_map(|i| match i {
            Inline::Run(r) if !r.text.is_empty() => Some((r.text.clone(), r.props.bold)),
            _ => None,
        })
        .collect()
}

/// The props of the run holding `ch`.
fn props_of(ed: &Editor, ch: char) -> RunProps {
    content(ed)
        .iter()
        .find_map(|i| match i {
            Inline::Run(r) if r.text.contains(ch) => Some(r.props.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no run holds {ch:?}"))
}

fn runs_of(spec: &[(&str, bool)]) -> Vec<(String, bool)> {
    spec.iter().map(|(t, b)| (t.to_string(), *b)).collect()
}

#[test]
fn bold_at_the_caret_bolds_what_is_typed_next_854() {
    let mut ed = empty();
    ed.insert_str("Abc");
    ed.toggle_bold();
    assert!(
        ed.caret_props().bold,
        "the ribbon shows Bold on at the caret"
    );
    assert_eq!(
        runs(&ed),
        runs_of(&[("Abc", false)]),
        "the text is unchanged"
    );
    ed.insert_str("def");
    ed.toggle_bold();
    assert!(!ed.caret_props().bold);
    ed.insert_str("ghi");
    assert_eq!(
        runs(&ed),
        runs_of(&[("Abc", false), ("def", true), ("ghi", false)])
    );
    let xml = document_to_xml(&ed.doc);
    assert_eq!(xml.matches("<w:b/>").count(), 1, "{xml}");
}

#[test]
fn each_toggle_at_the_caret_is_an_undo_step_854() {
    let mut ed = empty();
    ed.insert_str("Abc");
    ed.toggle_bold();
    ed.insert_str("def");
    ed.toggle_bold();
    ed.insert_str("ghi");
    assert_eq!(
        ed.undo_names(),
        [
            "Typing \"ghi\"",
            "Edit",
            "Typing \"def\"",
            "Edit",
            "Typing \"Abc\""
        ]
    );

    // Undo takes back only `ghi`; the second toggle still holds.
    assert!(ed.undo());
    assert_eq!(runs(&ed), runs_of(&[("Abc", false), ("def", true)]));
    assert!(!ed.caret_props().bold, "Bold is still switched off");
    // Undoing the second toggle: what is typed now is bold, like `def`.
    assert!(ed.undo());
    assert!(ed.caret_props().bold);
    // Undoing `def` brings back the first toggle, still switched on.
    assert!(ed.undo());
    assert_eq!(runs(&ed), runs_of(&[("Abc", false)]));
    assert!(ed.caret_props().bold, "the first toggle holds again");
    // Undoing the first toggle switches it off.
    assert!(ed.undo());
    assert!(!ed.caret_props().bold);
    // Redo switches it back on, and typing takes it.
    assert!(ed.redo());
    assert!(ed.caret_props().bold);
    ed.insert_char('x');
    assert_eq!(runs(&ed), runs_of(&[("Abc", false), ("x", true)]));
}

#[test]
fn toggling_twice_at_the_caret_types_plain_854() {
    let mut ed = on("Abc");
    ed.toggle_bold();
    ed.toggle_bold();
    assert!(!ed.caret_props().bold);
    assert_eq!(ed.undo_names(), ["Edit", "Edit"]);
    ed.insert_str("de");
    assert_eq!(runs(&ed), runs_of(&[("Abcde", false)]));
}

#[test]
fn moving_the_caret_leaves_the_toggle_behind_854() {
    let mut ed = on("Abc");
    ed.toggle_bold();
    assert!(ed.caret_props().bold);
    ed.set_caret(Caret::at(vec![0], 1));
    assert!(!ed.caret_props().bold);
    ed.insert_char('x');
    assert_eq!(runs(&ed), runs_of(&[("Axbc", false)]));
}

#[test]
fn selecting_leaves_the_toggle_behind_854() {
    let mut ed = on("Abc");
    ed.toggle_bold();
    assert!(ed.caret_props().bold);
    ed.anchor = Some(Caret::at(vec![0], 1));
    assert!(!ed.caret_props().bold);
    ed.insert_char('x');
    assert_eq!(runs(&ed), runs_of(&[("Ax", false)]));
}

#[test]
fn another_edit_leaves_the_toggle_behind_854() {
    // Delete forward keeps the caret where it was.
    let mut ed = on("Abcdef");
    ed.caret = Caret::at(vec![0], 3);
    ed.toggle_bold();
    assert!(ed.caret_props().bold);
    ed.delete_forward();
    assert_eq!(ed.caret.offset, 3);
    assert!(!ed.caret_props().bold);
    ed.insert_char('x');
    assert_eq!(runs(&ed), runs_of(&[("Abcxef", false)]));
}

#[test]
fn a_review_edit_leaves_the_toggle_behind_854() {
    // Accepting a revision pushes its step without a checkpoint and leaves
    // the caret where it is.
    let mut ed = Editor::new(parse(
        "<w:p><w:r><w:t>Abc</w:t></w:r>\
         <w:ins w:id=\"1\" w:author=\"Bo\" w:date=\"2026-01-01T00:00:00Z\">\
         <w:r><w:t>def</w:t></w:r></w:ins></w:p>",
    ));
    ed.caret = Caret::at(vec![0], 1);
    ed.toggle_bold();
    assert!(ed.caret_props().bold);
    assert!(!ed.accept_all_revisions().is_empty());
    assert_eq!(ed.caret, Caret::at(vec![0], 1));
    assert!(!ed.caret_props().bold);
    ed.insert_char('x');
    assert!(!props_of(&ed, 'x').bold);
}

#[test]
fn italic_underline_and_strike_at_the_caret_854() {
    let mut ed = empty();
    ed.toggle_italic();
    ed.insert_char('i');
    ed.toggle_italic();
    ed.toggle_underline();
    ed.insert_char('u');
    ed.toggle_underline();
    ed.toggle_strike();
    ed.insert_char('s');
    let (i, u, s) = (props_of(&ed, 'i'), props_of(&ed, 'u'), props_of(&ed, 's'));
    assert!(i.italic && !i.user_underline() && !i.user_strike());
    assert!(!u.italic && u.user_underline() && !u.user_strike());
    assert!(!s.italic && !s.user_underline() && s.user_strike());
}

#[test]
fn toggles_at_the_caret_combine_854() {
    let mut ed = on("Abc");
    ed.toggle_bold();
    ed.toggle_italic();
    let at = ed.caret_props();
    assert!(at.bold && at.italic);
    ed.insert_str("de");
    let d = props_of(&ed, 'd');
    assert!(d.bold && d.italic);
    assert!(!props_of(&ed, 'A').bold);
}

#[test]
fn a_tab_or_line_break_takes_the_toggle_and_text_after_it_too_854() {
    let mut ed = on("Abc");
    ed.toggle_bold();
    ed.insert_tab();
    ed.insert_char('d');
    ed.toggle_bold();
    ed.insert_line_break();
    ed.insert_char('e');
    let tab = content(&ed).iter().find_map(|i| match i {
        Inline::Tab(p) => Some(p.bold),
        _ => None,
    });
    let brk = content(&ed).iter().find_map(|i| match i {
        Inline::Break(_, p) => Some(p.bold),
        _ => None,
    });
    assert_eq!(tab, Some(true), "the tab is bold");
    assert!(props_of(&ed, 'd').bold, "text after the tab is bold");
    assert_eq!(brk, Some(false), "the break is not");
    assert!(!props_of(&ed, 'e').bold, "nor text after it");
}

#[test]
fn a_toggle_at_the_caret_with_track_changes_on_854() {
    let mut ed = on("Abc");
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ada".into(),
        clock: || "2026-03-04T05:06:07Z".to_string(),
    }));
    ed.toggle_bold();
    ed.insert_char('x');
    let x = props_of(&ed, 'x');
    assert!(x.bold, "the typed text is bold");
    assert!(x.tracked_insert.is_some(), "and recorded as an insertion");
    let xml = document_to_xml(&ed.doc);
    assert!(xml.contains("<w:ins "), "{xml}");
    assert_eq!(xml.matches("<w:b/>").count(), 1, "{xml}");
}

#[test]
fn a_toggle_over_a_selection_is_unchanged_854() {
    let mut ed = on("Abcdef");
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 3);
    ed.toggle_bold();
    assert_eq!(
        runs(&ed),
        runs_of(&[("A", false), ("bc", true), ("def", false)])
    );
    assert_eq!(ed.undo_names(), ["Edit"]);
}
