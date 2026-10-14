//! An equation and an inline picture are one editor offset each (#1242):
//! the caret stands on either side of one, Backspace and Delete reach it,
//! and typing goes on either side of it, never into it.

use super::*;
use crate::load::{Relationships, parse_document_xml};
use crate::serialize::document_to_xml;

pub(super) const PIC: &str = "<w:r><w:drawing><wp:inline><wp:extent cx=\"952500\" cy=\"952500\"/>\
    <a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId5\"/></pic:blipFill>\
    </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>";
const EQ: &str = "<m:oMath><m:r><m:t>x+1</m:t></m:r></m:oMath>";
const ANCHORED: &str = "<w:r><w:drawing><wp:anchor><wp:extent cx=\"952500\" cy=\"952500\"/>\
    </wp:anchor></w:drawing></w:r>";
const NO_EXTENT: &str = "<w:r><w:drawing><wp:inline/></w:drawing></w:r>";
const PTAB: &str =
    "<w:r><w:ptab w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"none\"/></w:r>";
const BOOKMARK: &str = "<w:bookmarkStart w:id=\"0\" w:name=\"b\"/>";

fn t(text: &str) -> String {
    format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>")
}

fn doc(paras: &[String]) -> Document {
    let body: String = paras.iter().map(|p| format!("<w:p>{p}</w:p>")).collect();
    parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &Relationships::default(),
    )
}

/// `ab<object>cd` in one paragraph.
fn ed_with(object: &str) -> Editor {
    Editor::new(doc(&[format!("{}{object}{}", t("ab"), t("cd"))]))
}

fn para(ed: &Editor, i: usize) -> &Paragraph {
    match &ed.doc.body[i] {
        Block::Paragraph(p) => p,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

fn etext(ed: &Editor) -> String {
    editor_text(&para(ed, 0).content)
}

fn saved(ed: &Editor) -> String {
    document_to_xml(&ed.doc)
}

fn select(ed: &mut Editor, path: Vec<usize>, start: usize, end: usize) {
    ed.anchor = Some(Caret::at(path.clone(), start));
    ed.caret = Caret::at(path, end);
}

fn track(ed: &mut Editor) {
    ed.set_track_changes(Some(TrackAuthor {
        author: "Ann".into(),
        clock: || "2026-10-15T00:00:00Z".into(),
    }));
}

/// Each inline of paragraph 0, by kind: runs by their text.
fn kinds(ed: &Editor) -> Vec<String> {
    para(ed, 0)
        .content
        .iter()
        .map(|i| match i {
            Inline::Run(r) => r.text.clone(),
            Inline::Equation { .. } => "[EQ]".into(),
            Inline::Raw(r) if is_object_unit(i) && r.contains("<w:drawing") => "[PIC]".into(),
            Inline::Revision {
                kind: RevisionKind::Delete,
                content,
                ..
            } => format!(
                "del({})",
                content
                    .iter()
                    .map(|c| match c {
                        Inline::Run(r) => r.text.clone(),
                        Inline::Raw(_) => "[PIC]".into(),
                        other => format!("{other:?}"),
                    })
                    .collect::<String>()
            ),
            Inline::Raw(_) => "raw".into(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn an_equation_or_inline_picture_is_one_offset_1242() {
    for (object, kind) in [(PIC, "[PIC]"), (EQ, "[EQ]")] {
        let ed = ed_with(object);
        assert_eq!(kinds(&ed), ["ab", kind, "cd"]);
        assert_eq!(etext(&ed), "ab\u{FFFC}cd");
        assert_eq!(inline_len(&para(&ed, 0).content[1]), 1);
        assert_eq!(para_text_len(para(&ed, 0)), 5);
        let flat = flat::FlatDocument::new(&ed.doc);
        assert_eq!(flat.main().text, "ab\u{FFFC}cd\n");
    }
    // A legacy Equation Editor object is an equation too, whatever its XML.
    let legacy = Inline::Equation {
        raw: "<w:r><w:object/></w:r>".into(),
        text: "a".into(),
        latex: None,
    };
    assert_eq!(inline_len(&legacy), 1);
    // Anchored, extent-less, a positional tab, a bookmark: zero-width.
    for object in [ANCHORED, NO_EXTENT, PTAB, BOOKMARK] {
        let ed = ed_with(object);
        assert_eq!(etext(&ed), "abcd", "{object}");
    }
    // (A chart takes the same arm as SmartArt.)
    for zero in [
        Inline::SmartArt {
            raw: PIC.into(),
            text: vec!["n".into()],
        },
        Inline::TextBox {
            raw: PIC.into(),
            blocks: Vec::new(),
        },
        Inline::Revision {
            kind: RevisionKind::Insert,
            metadata: Default::default(),
            raw: "<w:ins/>".into(),
            content: vec![Inline::Raw(PIC.into())],
            content_changed: false,
        },
    ] {
        assert_eq!(inline_len(&zero), 0, "{zero:?}");
    }
}

#[test]
fn backspace_deletes_a_picture_at_once_1242() {
    let mut ed = ed_with(PIC);
    let before = ed.doc.clone();
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    assert_eq!(kinds(&ed), ["ab", "cd"]);
    assert_eq!(ed.caret.offset, 2);
    assert!(!saved(&ed).contains("<w:drawing"));
    assert!(ed.undo());
    assert_eq!(ed.doc, before, "undo brings the picture back");
    assert!(saved(&ed).contains(PIC));

    ed.set_caret(Caret::at(vec![0], 2));
    ed.delete_forward();
    assert_eq!(kinds(&ed), ["ab", "cd"]);
    assert_eq!(ed.caret.offset, 2);
}

#[test]
fn backspace_selects_an_equation_then_deletes_it_1242() {
    let mut ed = ed_with(EQ);
    let before = ed.doc.clone();
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    let (lo, hi) = ed.selection_range().expect("the equation is selected");
    assert_eq!((lo.offset, hi.offset), (2, 3));
    assert_eq!(ed.doc, before, "selecting changes nothing");
    ed.backspace();
    assert_eq!(kinds(&ed), ["ab", "cd"]);
    assert!(!saved(&ed).contains("m:oMath"));
    assert!(ed.undo());
    assert_eq!(ed.doc, before);

    ed.set_caret(Caret::at(vec![0], 2));
    ed.delete_forward();
    let (lo, hi) = ed.selection_range().expect("the equation is selected");
    assert_eq!((lo.offset, hi.offset), (2, 3));
    ed.delete_forward();
    assert_eq!(kinds(&ed), ["ab", "cd"]);
}

#[test]
fn typing_goes_on_either_side_of_an_object_1242() {
    for (object, kind) in [(PIC, "[PIC]"), (EQ, "[EQ]")] {
        let mut ed = ed_with(object);
        ed.set_caret(Caret::at(vec![0], 2));
        ed.insert_char('X');
        assert_eq!(kinds(&ed), ["abX", kind, "cd"]);
        let mut ed = ed_with(object);
        ed.set_caret(Caret::at(vec![0], 3));
        ed.insert_char('X');
        assert_eq!(kinds(&ed), ["ab", kind, "Xcd"]);
        assert_eq!(ed.caret.offset, 4);
        // The object's own XML is untouched.
        let xml = saved(&ed);
        let open = if object == PIC {
            "<w:drawing"
        } else {
            "<m:oMath>"
        };
        let close = if object == PIC {
            "</w:drawing>"
        } else {
            "</m:oMath>"
        };
        let (a, b) = (xml.find(open).unwrap(), xml.find(close).unwrap());
        assert!(!xml[a..b].contains('X'), "{xml}");
        assert!(xml.find('X').unwrap() > b, "X after the object: {xml}");
        // The stand-in is never typed.
        ed.insert_char(FIELD_CHAR);
        assert_eq!(etext(&ed), "ab\u{FFFC}Xcd");
    }
    // At the paragraph's start, before a leading object.
    let mut ed = Editor::new(doc(&[format!("{PIC}{}", t("cd"))]));
    ed.set_caret(Caret::at(vec![0], 0));
    ed.insert_char('X');
    assert_eq!(kinds(&ed), ["X", "[PIC]", "cd"]);
}

#[test]
fn text_typed_after_an_object_takes_the_text_before_it_1242() {
    let bold = "<w:r><w:rPr><w:b/></w:rPr><w:t>ab</w:t></w:r>";
    let mut ed = Editor::new(doc(&[format!("{bold}{PIC}{}", t("cd"))]));
    ed.set_caret(Caret::at(vec![0], 3));
    ed.insert_char('X');
    assert_eq!(kinds(&ed), ["ab", "[PIC]", "X", "cd"]);
    let Inline::Run(x) = &para(&ed, 0).content[2] else {
        panic!("a run");
    };
    assert!(
        x.props.bold,
        "X takes the bold of the text before the picture"
    );
}

#[test]
fn copy_and_cut_carry_the_object_1242() {
    for (object, text) in [(PIC, ""), (EQ, "x+1")] {
        let mut ed = ed_with(object);
        select(&mut ed, vec![0], 2, 3);
        let clip = ed.copy().expect("a clip");
        assert_eq!(clip.paras.len(), 1);
        assert!(is_object_unit(&clip.paras[0][0]), "{clip:?}");
        assert_eq!(ed.selection_text(), text);
        // Pasting it reproduces the object.
        ed.anchor = None;
        ed.set_caret(Caret::at(vec![0], 5));
        ed.paste(&clip);
        assert_eq!(etext(&ed), "ab\u{FFFC}cd\u{FFFC}");
        // A cut moves it.
        let mut ed = ed_with(object);
        select(&mut ed, vec![0], 1, 4);
        let clip = ed.cut().expect("a clip");
        assert_eq!(etext(&ed), "ad");
        assert!(!saved(&ed).contains(&object[..10]));
        ed.paste(&clip);
        assert_eq!(etext(&ed), "ab\u{FFFC}cd");
    }
}

/// #768's rule takes no part in a one-offset picture: a delete through its
/// offset removes it and exactly the characters selected beside it.
#[test]
fn a_range_delete_over_a_picture_deletes_exactly_the_range_1242() {
    let mut ed = ed_with(PIC);
    select(&mut ed, vec![0], 1, 4);
    assert!(ed.delete_selection());
    assert_eq!(kinds(&ed), ["a", "d"]);
    // Across paragraphs: from mid-paragraph 1 to just after the picture.
    let mut ed = Editor::new(doc(&[t("abc"), format!("{}{PIC}{}", t("de"), t("fg"))]));
    select(&mut ed, vec![0], 1, 0);
    ed.caret = Caret::at(vec![1], 3);
    assert!(ed.delete_selection());
    assert_eq!(ed.doc.body.len(), 1);
    assert_eq!(kinds(&ed), ["a", "fg"]);
    // An anchored drawing keeps #768's rule: the delete over it removes it.
    let mut ed = ed_with(ANCHORED);
    select(&mut ed, vec![0], 1, 3);
    assert!(ed.delete_selection());
    assert_eq!(kinds(&ed), ["a", "d"]);
    assert!(!saved(&ed).contains("<w:drawing"));
}

#[test]
fn deleting_the_only_picture_in_a_link_removes_the_link_1242() {
    let mut ed = Editor::new(doc(&[format!(
        "{}<w:hyperlink r:id=\"rId9\">{PIC}</w:hyperlink>",
        t("ab")
    )]));
    assert_eq!(etext(&ed), "ab\u{FFFC}");
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    assert_eq!(etext(&ed), "ab");
    assert!(
        !para(&ed, 0)
            .content
            .iter()
            .any(|i| matches!(i, Inline::Hyperlink(_))),
        "{:?}",
        para(&ed, 0).content
    );
}

#[test]
fn a_tracked_delete_of_a_picture_is_recorded_1242() {
    let mut ed = ed_with(PIC);
    track(&mut ed);
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    assert_eq!(kinds(&ed), ["ab", "del([PIC])", "cd"]);
    assert_eq!(ed.caret.offset, 2);
    let xml = saved(&ed);
    let del = xml.find("<w:del ").expect("a w:del");
    assert!(xml[del..].find("<w:drawing").is_some(), "{xml}");
    assert!(xml.contains(PIC), "the picture's run is kept whole: {xml}");
    // A deletion of the text just before it is a revision of its own.
    ed.backspace();
    assert_eq!(kinds(&ed), ["a", "del(b)", "del([PIC])", "cd"]);
    // Reject brings the picture back; accept removes it.
    let mut rejected = Editor::new(ed.doc.clone());
    rejected.reject_all_revisions();
    assert_eq!(etext(&rejected), "ab\u{FFFC}cd");
    assert!(saved(&rejected).contains(PIC));
    ed.accept_all_revisions();
    assert_eq!(kinds(&ed), ["a", "cd"]);
    assert!(!saved(&ed).contains("<w:drawing"));
}

#[test]
fn a_tracked_delete_leaves_an_equation_1242() {
    let mut ed = ed_with(EQ);
    track(&mut ed);
    // Backspace selects it; the second press leaves it selected.
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    ed.backspace();
    assert_eq!(kinds(&ed), ["ab", "[EQ]", "cd"]);
    let (lo, hi) = ed.selection_range().expect("still selected");
    assert_eq!((lo.offset, hi.offset), (2, 3));
    ed.delete_forward();
    assert_eq!(kinds(&ed), ["ab", "[EQ]", "cd"]);
    // A range over it records the text and steps over the equation:
    // nothing after the range is touched.
    select(&mut ed, vec![0], 1, 4);
    assert!(ed.delete_selection());
    assert_eq!(kinds(&ed), ["a", "del(b)", "[EQ]", "del(c)", "d"]);
}

#[test]
fn typing_beside_a_display_equation_stays_outside_it_1242() {
    let display = "<m:oMathPara><m:oMath><m:r><m:t>y</m:t></m:r></m:oMath></m:oMathPara>";
    let mut ed = Editor::new(doc(&[display.to_string()]));
    assert_eq!(etext(&ed), "\u{FFFC}");
    ed.set_caret(Caret::at(vec![0], 1));
    ed.insert_char('X');
    ed.set_caret(Caret::at(vec![0], 0));
    ed.insert_char('W');
    assert_eq!(kinds(&ed), ["W", "[EQ]", "X"]);
    let xml = saved(&ed);
    let (a, b) = (
        xml.find("<m:oMathPara>").unwrap(),
        xml.find("</m:oMathPara>").unwrap(),
    );
    assert!(!xml[a..b].contains("<w:r"), "{xml}");
    assert!(xml.find(">W<").unwrap() < a && xml.find(">X<").unwrap() > b);
}

/// A loose field whose result is an inline picture (INCLUDEPICTURE, kept
/// loose by #642): Backspace after it takes the whole field, and typing at
/// its end goes past its end marker (#1240).
#[test]
fn a_loose_field_showing_a_picture_is_edited_whole_1242() {
    let include = format!(
        "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:instrText> INCLUDEPICTURE \"a.png\" </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>{PIC}\
         <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
    );
    let mut ed = Editor::new(doc(&[format!("{}{include}{}", t("ab"), t("cd"))]));
    assert_eq!(etext(&ed), "ab\u{FFFC}cd");
    ed.set_caret(Caret::at(vec![0], 3));
    ed.insert_char('X');
    let xml = saved(&ed);
    assert!(
        xml.find(">Xcd<").unwrap() > xml.find("w:fldCharType=\"end\"").unwrap(),
        "typed past the field's end: {xml}"
    );
    let mut ed = Editor::new(doc(&[format!("{}{include}{}", t("ab"), t("cd"))]));
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    if ed.has_selection() {
        ed.backspace();
    }
    assert_eq!(etext(&ed), "abcd");
    let xml = saved(&ed);
    for gone in ["fldChar", "instrText", "<w:drawing"] {
        assert!(!xml.contains(gone), "{gone} left: {xml}");
    }
}

/// A host's plain-text cut (the browser's) cannot carry an equation or an
/// inline picture: they stay where the cut was, its text goes (#768 r2),
/// and an equation kept gives no text (FIX r1 M3), so a paste does not
/// bring it back as text beside it.
#[test]
fn a_plain_text_cut_keeps_the_objects_its_text_cannot_carry_1242() {
    for (object, kind) in [(PIC, "[PIC]"), (EQ, "[EQ]")] {
        let text = "bc";
        let mut ed = ed_with(object);
        select(&mut ed, vec![0], 1, 4);
        assert_eq!(ed.cut_text().as_deref(), Some(text));
        assert_eq!(kinds(&ed), ["a", kind, "d"]);
        assert!(ed.undo());
        assert_eq!(etext(&ed), "ab\u{FFFC}cd");
        // Across paragraphs, in the first and the last.
        let mut ed = Editor::new(doc(&[
            format!("{}{object}{}", t("ab"), t("cd")),
            format!("{}{object}{}", t("ef"), t("gh")),
        ]));
        select(&mut ed, vec![0], 1, 0);
        ed.caret = Caret::at(vec![1], 4);
        assert!(ed.cut_text().is_some());
        assert_eq!(ed.doc.body.len(), 1);
        assert_eq!(kinds(&ed), ["a", kind, kind, "h"]);
    }
    // A lone picture or equation: nothing to copy, nothing done.
    for object in [PIC, EQ] {
        let mut ed = ed_with(object);
        select(&mut ed, vec![0], 2, 3);
        assert_eq!(ed.cut_text(), None);
        assert_eq!(etext(&ed), "ab\u{FFFC}cd");
        assert!(!ed.undo(), "no step");
    }
    // A paragraph wholly inside the cut goes whole, its equation and the
    // equation's text with it.
    let mut ed = Editor::new(doc(&[t("ab"), format!("{EQ}{}", t("x")), t("cd")]));
    select(&mut ed, vec![0], 1, 0);
    ed.caret = Caret::at(vec![2], 1);
    assert_eq!(ed.cut_text().as_deref(), Some("b\nx+1x\nc"));
    assert_eq!(etext(&ed), "ad");
    // While tracking every equation stays, so none gives text.
    let mut ed = Editor::new(doc(&[t("ab"), format!("{EQ}{}", t("x")), t("cd")]));
    track(&mut ed);
    select(&mut ed, vec![0], 1, 0);
    ed.caret = Caret::at(vec![2], 1);
    assert_eq!(ed.cut_text().as_deref(), Some("b\nx\nc"));
    assert!(saved(&ed).contains("<m:oMath>"));
}

/// FIX r1 M1: the text after an equation or a picture sits one offset
/// later in a formatting toggle's check too, so Ctrl+B over bold text there
/// unbolds it.
#[test]
fn a_toggle_after_an_object_reads_the_right_characters_1242() {
    for object in [EQ, PIC] {
        let mut ed = Editor::new(doc(&[format!(
            "{}{object}<w:r><w:rPr><w:b/></w:rPr><w:t>cd</w:t></w:r>{}",
            t("ab"),
            t("ef")
        )]));
        // "cd", all bold.
        select(&mut ed, vec![0], 3, 5);
        ed.toggle_bold();
        let bold: String = para(&ed, 0)
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) if r.props.bold => Some(r.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bold, "", "{object}");
    }
}
