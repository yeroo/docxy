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
            Inline::Revision { kind, content, .. } => format!(
                "{}({})",
                match kind {
                    RevisionKind::Delete => "del",
                    RevisionKind::Insert => "ins",
                },
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

/// FIX r1 M4: while tracking, a cut picture is a `w:del` where it was and
/// its paste a `w:ins` where it goes, so Reject All leaves the one picture
/// where it was and Accept All the one where it went.
#[test]
fn a_tracked_cut_and_paste_moves_a_picture_as_two_revisions_1242() {
    let mut ed = ed_with(PIC);
    track(&mut ed);
    select(&mut ed, vec![0], 2, 3);
    let clip = ed.cut().expect("a clip");
    assert_eq!(kinds(&ed), ["ab", "del([PIC])", "cd"]);
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![0], 4));
    ed.paste(&clip);
    assert_eq!(kinds(&ed), ["ab", "del([PIC])", "cd", "ins([PIC])"]);
    assert_eq!(ed.caret.offset, 4, "a pending insertion takes no offset");
    let xml = saved(&ed);
    let ins = xml.find("<w:ins ").expect("a w:ins");
    assert!(xml[ins..].contains("<w:drawing"), "{xml}");
    assert_eq!(xml.matches(PIC).count(), 2, "{xml}");
    let mut rejected = Editor::new(ed.doc.clone());
    rejected.reject_all_revisions();
    assert_eq!(etext(&rejected), "ab\u{FFFC}cd");
    ed.accept_all_revisions();
    assert_eq!(etext(&ed), "abcd\u{FFFC}");
    assert_eq!(saved(&ed).matches(PIC).count(), 1);

    // Text and a picture pasted together: each w:ins has an id of its own.
    let mut ed = ed_with(PIC);
    select(&mut ed, vec![0], 1, 4);
    let clip = ed.copy().expect("a clip");
    track(&mut ed);
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![0], 5));
    ed.paste(&clip);
    let xml = saved(&ed);
    let ids: Vec<&str> = xml
        .match_indices("<w:ins ")
        .map(|(at, _)| {
            let id = &xml[at..][xml[at..].find("w:id=\"").unwrap() + 6..];
            &id[..id.find('"').unwrap()]
        })
        .collect();
    assert!(ids.len() >= 2, "{xml}");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "{xml}");
}

/// FIX r1 M4: while tracking an equation stays where it was, so a cut
/// leaves it out of its clip and a paste adds none: never two.
#[test]
fn a_tracked_cut_and_paste_never_doubles_an_equation_1242() {
    let eqs = |ed: &Editor| saved(ed).matches("<m:oMath>").count();
    let mut ed = ed_with(EQ);
    track(&mut ed);
    select(&mut ed, vec![0], 1, 4);
    let clip = ed.cut().expect("a clip");
    assert_eq!(kinds(&ed), ["a", "del(b)", "[EQ]", "del(c)", "d"]);
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![0], 3));
    ed.paste(&clip);
    assert_eq!(eqs(&ed), 1, "{}", saved(&ed));
    // A copy pasted while tracking leaves the equation out too.
    let mut ed = ed_with(EQ);
    select(&mut ed, vec![0], 2, 3);
    let clip = ed.copy().expect("a clip");
    track(&mut ed);
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![0], 5));
    ed.paste(&clip);
    assert_eq!(eqs(&ed), 1);
    // A new equation still goes in.
    ed.insert_equation("y", false);
    assert_eq!(eqs(&ed), 2);
}

/// FIX r3: while tracking, a copy (or any reader of the selection's text)
/// still gives an equation's text; only a plain-text cut leaves it out.
#[test]
fn a_tracked_copy_gives_an_equations_text_1242() {
    let mut ed = ed_with(EQ);
    track(&mut ed);
    select(&mut ed, vec![0], 1, 4);
    assert_eq!(ed.selection_text(), "bx+1c");
    assert_eq!(ed.cut_text().as_deref(), Some("bc"));
}

/// FIX r4: Find sees a multi-line equation in a link as render.rs draws it,
/// its lines joined in the line, at its one unit.
#[test]
fn find_sees_a_linked_multi_line_equation_at_its_unit_1242() {
    let link = Inline::Hyperlink(crate::model::Hyperlink {
        target: Some("https://x.test/".into()),
        content: vec![Inline::Equation {
            raw: "<m:oMath/>".into(),
            text: "a b\nc d".into(),
            latex: None,
        }],
        ..Default::default()
    });
    let ed = Editor::new(Document {
        body: vec![Block::Paragraph(Paragraph {
            props: Default::default(),
            content: vec![
                Inline::Run(Run {
                    text: "x".into(),
                    props: Default::default(),
                }),
                link,
            ],
        })],
    });
    let found = ed.find_visible("b c", false);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].start, found[0].end), (1, 2));
    assert!(!found[0].editable);

    // FIX r5: an equation with no text shows, and is found, as its label.
    let ed = Editor::new(doc(&[format!("{}<m:oMath/>{}", t("ab"), t("cd"))]));
    assert_eq!(etext(&ed), "ab\u{FFFC}cd");
    let found = ed.find_visible("equation", false);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].start, found[0].end), (2, 3));
}

/// FIX r4 m1: a tracked cut of a lone equation (which stays) has nothing to
/// cut: no clip, no step, the document as it was.
#[test]
fn a_tracked_cut_of_a_lone_equation_is_refused_1242() {
    let mut ed = ed_with(EQ);
    track(&mut ed);
    let before = ed.doc.clone();
    select(&mut ed, vec![0], 2, 3);
    assert_eq!(ed.cut(), None);
    assert_eq!(ed.doc, before);
    assert!(!ed.undo(), "no step");
    // With text beside it the cut goes on, the equation staying.
    select(&mut ed, vec![0], 1, 3);
    assert!(ed.cut().is_some());
    assert_eq!(kinds(&ed), ["a", "del(b)", "[EQ]", "cd"]);
}

/// `ab` / `c<eq>d` / `ef` and a 1×2 table whose second cell holds `x<eq>`.
fn three_paras_and_a_table() -> Editor {
    let cell = |inner: &str| format!("<w:tc><w:p>{inner}</w:p></w:tc>");
    Editor::new(parse_document_xml(
        &format!(
            "<w:document><w:body><w:p>{}</w:p><w:p>{}{EQ}{}</w:p><w:p>{}</w:p>\
             <w:tbl><w:tr>{}{}</w:tr></w:tbl><w:p/></w:body></w:document>",
            t("ab"),
            t("c"),
            t("d"),
            t("ef"),
            cell(&t("y")),
            cell(&format!("{}{EQ}", t("x"))),
        ),
        &Relationships::default(),
    ))
}

/// FIX r2 M1: the text of a plain-text cut leaves out exactly the equations
/// its delete keeps, whichever spans the selection has: one in a paragraph
/// it removes whole, or in a cell range it clears, is copied, never lost.
#[test]
fn a_plain_text_cut_copies_every_equation_it_removes_1242() {
    let eqs = |ed: &Editor| saved(ed).matches("<m:oMath>").count();
    // Ending at the start of the paragraph after the equation's: the
    // equation's paragraph goes whole, its text copied.
    let mut ed = three_paras_and_a_table();
    select(&mut ed, vec![0], 1, 0);
    ed.caret = Caret::at(vec![2], 0);
    let text = ed.cut_text().expect("text");
    assert!(text.contains("cx+1d"), "{text:?}");
    assert_eq!(eqs(&ed), 1, "only the table's left");
    // Starting at the end of the paragraph before it.
    let mut ed = three_paras_and_a_table();
    select(&mut ed, vec![0], 2, 0);
    ed.caret = Caret::at(vec![2], 1);
    let text = ed.cut_text().expect("text");
    assert!(text.contains("cx+1d"), "{text:?}");
    assert_eq!(eqs(&ed), 1);
    // Over a cell range: the cells are cleared, the equation's text copied.
    let mut ed = three_paras_and_a_table();
    select(&mut ed, vec![3, 0, 0, 0], 0, 0);
    ed.caret = Caret::at(vec![3, 0, 1, 0], 1);
    assert!(ed.cell_range().is_some());
    let text = ed.cut_text().expect("text");
    assert!(text.contains("xx+1"), "{text:?}");
    assert_eq!(eqs(&ed), 1, "only the paragraph's left");
}

/// FIX r2 M2, then #1471: a tracked cut over a cell range records the
/// cells' text as deleted and keeps their equations in place, as a tracked
/// paragraph cut does, so its clip leaves them out: a paste must not add a
/// second.
#[test]
fn a_tracked_cut_of_cells_keeps_their_equations_1242() {
    let has_eq = |clip: &Clip| {
        clip.paras
            .iter()
            .flatten()
            .any(|i| matches!(i, Inline::Equation { .. }))
    };
    let mut ed = three_paras_and_a_table();
    track(&mut ed);
    select(&mut ed, vec![3, 0, 0, 0], 0, 0);
    ed.caret = Caret::at(vec![3, 0, 1, 0], 1);
    let clip = ed.cut().expect("a clip");
    assert!(!has_eq(&clip), "{clip:?}");
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1, "kept in its cell");
    // And a paragraph cut while tracking still leaves its equation out.
    let mut ed = three_paras_and_a_table();
    track(&mut ed);
    select(&mut ed, vec![1], 0, 3);
    let clip = ed.cut().expect("a clip");
    assert!(!has_eq(&clip));
}

/// The equations in the paragraph at `path`.
fn cell_equations(ed: &Editor, path: Vec<usize>) -> usize {
    resolve_para(&ed.doc.body, &path)
        .expect("a paragraph")
        .content
        .iter()
        .filter(|i| matches!(i, Inline::Equation { .. }))
        .count()
}

/// The text of the paragraph at `path` with its deletions, as Reject would
/// leave it.
fn para_plain(ed: &Editor, path: Vec<usize>) -> String {
    resolve_para(&ed.doc.body, &path)
        .expect("a paragraph")
        .plain_text()
}

/// #1471: a tracked cut of table cells, then a tracked paste, keeps their
/// equations (it lost them: the cells were cleared untracked, the clip
/// carried the equations and the tracked paste left them out), and Reject
/// All brings the cells back and takes the paste away.
#[test]
fn a_tracked_cut_and_paste_of_cells_keeps_their_equations_1471() {
    let eqs = |ed: &Editor| saved(ed).matches("<m:oMath>").count();
    let mut ed = three_paras_and_a_table();
    let original = saved(&ed);
    assert_eq!(eqs(&ed), 2);
    track(&mut ed);
    select(&mut ed, vec![3, 0, 0, 0], 0, 0);
    ed.caret = Caret::at(vec![3, 0, 1, 0], 1);
    let clip = ed.cut().expect("a clip");
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![4], 0));
    ed.paste(&clip);
    let xml = saved(&ed);
    assert_eq!(eqs(&ed), 2, "{xml}");
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1, "{xml}");
    let dels = deleted_objects(&xml);
    assert!(dels.iter().any(|d| d.contains(">y<")), "{dels:?}");
    assert!(dels.iter().any(|d| d.contains(">x<")), "{dels:?}");
    // One clip paragraph per cell; the pasted text is a tracked insertion.
    assert_eq!(para_plain(&ed, vec![4]), "y");
    assert_eq!(para_plain(&ed, vec![5]), "x");
    assert_eq!(insertions(&xml).len(), 2, "{xml}");
    // Reject All: the table exactly as it was, the pasted text gone (the
    // paste's paragraph mark stays: a tracked paste does not record one).
    ed.reject_all_revisions();
    assert_eq!(para_plain(&ed, vec![3, 0, 0, 0]), "y");
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1);
    assert_eq!(etext_at(&ed, vec![3, 0, 1, 0]), "x\u{FFFC}");
    let table =
        |xml: &str| xml[xml.find("<w:tbl>").unwrap()..xml.find("</w:tbl>").unwrap()].to_string();
    assert_eq!(table(&saved(&ed)), table(&original));
    assert_eq!(para_plain(&ed, vec![4]), "");
    assert_eq!(para_plain(&ed, vec![5]), "");
    assert_eq!(eqs(&ed), 2);
}

/// The `w:ins` wrappers in the saved XML.
fn insertions(xml: &str) -> Vec<&str> {
    xml.match_indices("<w:ins ")
        .map(|(at, _)| &xml[at..])
        .collect()
}

/// The editor text of the paragraph at `path` (an equation is one
/// [`FIELD_CHAR`]-like offset).
fn etext_at(ed: &Editor, path: Vec<usize>) -> String {
    editor_text(
        &resolve_para(&ed.doc.body, &path)
            .expect("a paragraph")
            .content,
    )
}

/// #1471: a tracked Delete over a cell range records the cells' text as
/// deleted, keeping their equations; Reject All restores the text, Accept
/// All removes it and keeps the equations, and Undo takes it back in one
/// step.
#[test]
fn a_tracked_delete_of_cells_is_recorded_1471() {
    let deleted = || {
        let mut ed = three_paras_and_a_table();
        track(&mut ed);
        select(&mut ed, vec![3, 0, 0, 0], 0, 0);
        ed.caret = Caret::at(vec![3, 0, 1, 0], 1);
        assert!(ed.delete_selection());
        ed
    };
    let original = saved(&three_paras_and_a_table());
    let mut ed = deleted();
    let dels = deleted_objects(&saved(&ed));
    assert_eq!(dels.len(), 2, "{dels:?}");
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1);
    assert_eq!(ed.caret, Caret::at(vec![3, 0, 0, 0], 0));
    assert!(ed.anchor.is_none());
    // Undo: one step back to the cells as they were.
    assert!(ed.undo());
    assert_eq!(saved(&ed), original);
    assert!(!ed.undo(), "one step");
    // Reject All restores the text.
    let mut ed = deleted();
    ed.reject_all_revisions();
    assert_eq!(saved(&ed), original);
    // Accept All empties the cells' text and keeps the equation.
    let mut ed = deleted();
    ed.accept_all_revisions();
    assert_eq!(para_plain(&ed, vec![3, 0, 0, 0]), "");
    assert_eq!(etext_at(&ed, vec![3, 0, 1, 0]), "\u{FFFC}");
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1);
}

/// A one-row table whose cells hold `cells` (each a cell's inner XML), then
/// an empty paragraph.
fn table_of(cells: &[String]) -> Editor {
    let tcs: String = cells.iter().map(|c| format!("<w:tc>{c}</w:tc>")).collect();
    Editor::new(parse_document_xml(
        &format!(
            "<w:document><w:body><w:tbl><w:tr>{tcs}</w:tr></w:tbl><w:p/></w:body></w:document>"
        ),
        &Relationships::default(),
    ))
}

/// #1471: a tracked clear keeps the cells' blocks, so the caret goes to the
/// first cell's first paragraph, here in a nested table, never to the
/// nested table itself; text typed over the range lands there.
#[test]
fn a_tracked_clear_puts_the_caret_on_a_paragraph_1471() {
    let nested = format!(
        "<w:tbl><w:tr><w:tc><w:p>{}</w:p></w:tc></w:tr></w:tbl><w:p>{}</w:p>",
        t("n"),
        t("y")
    );
    let mut ed = table_of(&[nested, format!("<w:p>{}</w:p>", t("x"))]);
    track(&mut ed);
    ed.anchor = Some(Caret::at(vec![0, 0, 0, 1], 0));
    ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
    assert!(ed.cell_range().is_some());
    ed.insert_char('z');
    let first = vec![0, 0, 0, 0, 0, 0, 0];
    assert_eq!(ed.caret, Caret::at(first.clone(), 1));
    assert!(resolve_para(&ed.doc.body, &ed.caret.path).is_some());
    assert_eq!(etext_at(&ed, first.clone()), "z");
    assert_eq!(para_plain(&ed, first), "zn");
    assert_eq!(para_plain(&ed, vec![0, 0, 0, 1]), "y");
    assert_eq!(etext_at(&ed, vec![0, 0, 0, 1]), "");
}

/// #1471: a tracked Delete over cells records a floating picture in them as
/// a `w:del` (it went with the cell's blocks before); a tracked cut keeps it
/// in place, as a paragraph cut does (the clip cannot carry it).
#[test]
fn a_tracked_cell_delete_records_a_floating_picture_1471() {
    let cells = || {
        let mut ed = table_of(&[
            format!("<w:p>{}{ANCHORED}{}</w:p>", t("a"), t("b")),
            format!("<w:p>{}</w:p>", t("c")),
        ]);
        track(&mut ed);
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
        ed
    };
    let mut ed = cells();
    assert!(ed.delete_selection());
    let dels = deleted_objects(&saved(&ed));
    assert!(dels.iter().any(|d| d.contains("wp:anchor")), "{dels:?}");
    ed.reject_all_revisions();
    let rejected = saved(&ed);
    assert!(rejected.contains("wp:anchor"), "{rejected}");
    assert!(deleted_objects(&rejected).is_empty(), "{rejected}");
    let mut ed = cells();
    ed.cut().expect("a clip");
    let xml = saved(&ed);
    assert!(xml.contains("wp:anchor"), "{xml}");
    assert!(
        !deleted_objects(&xml)
            .iter()
            .any(|d| d.contains("wp:anchor")),
        "{xml}"
    );
}

/// A text box (an inline drawing) holding one paragraph of `text`.
fn text_box(text: &str) -> String {
    format!(
        "<w:r><w:drawing><wp:inline><wp:extent cx=\"952500\" cy=\"952500\"/>\
         <w:txbxContent><w:p>{}</w:p></w:txbxContent></wp:inline></w:drawing></w:r>",
        t(text)
    )
}

/// The paragraph of the `nth` text box in the paragraph at `host`, found
/// afresh (its path holds the box's inline index, which edits move).
fn box_para(ed: &Editor, host: &[usize], nth: usize) -> Vec<usize> {
    let p = resolve_para(&ed.doc.body, host).expect("a host paragraph");
    let k = p
        .content
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, Inline::TextBox { .. }))
        .nth(nth)
        .expect("a text box")
        .0;
    let mut path = host.to_vec();
    path.extend([k, 0]);
    path
}

/// FIX r1 M1: a tracked clear walks a cell's text-box paragraphs before
/// their host, whose edit (two differently formatted runs recorded as one
/// deletion) moves the box's inline index: the box's text is recorded, not
/// left live, so a cut and paste brings it back once.
#[test]
fn a_tracked_clear_records_a_text_box_in_a_cell_1471() {
    let host = vec![0, 0, 0, 0];
    let cells = || {
        let bold = "<w:r><w:rPr><w:b/></w:rPr><w:t>A</w:t></w:r>";
        let mut ed = table_of(&[
            format!("<w:p>{bold}{}{}</w:p>", t("b"), text_box("box")),
            format!("<w:p>{}</w:p>", t("c")),
        ]);
        track(&mut ed);
        ed.anchor = Some(Caret::at(host.clone(), 0));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
        ed
    };
    let original = saved(&cells());
    let table =
        |xml: &str| xml[xml.find("<w:tbl>").unwrap()..xml.find("</w:tbl>").unwrap()].to_string();
    let boxed = |ed: &Editor| {
        let path = box_para(ed, &host, 0);
        (etext_at(ed, path.clone()), para_plain(ed, path))
    };
    // A tracked Delete records the box's text.
    let mut ed = cells();
    assert!(ed.delete_selection());
    assert_eq!(boxed(&ed), (String::new(), "box".into()), "{}", saved(&ed));
    assert!(
        deleted_objects(&saved(&ed))
            .iter()
            .any(|d| d.contains(">box<"))
    );
    // A tracked cut, then a tracked paste: the box's text is deleted in the
    // cell and inserted once by the paste.
    let mut ed = cells();
    let clip = ed.cut().expect("a clip");
    assert_eq!(clip.to_text(), "Ab\nbox\nc");
    assert_eq!(boxed(&ed), (String::new(), "box".into()), "{}", saved(&ed));
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![1], 0));
    ed.paste(&clip);
    let live = |ed: &Editor| {
        all_paragraph_paths(&ed.doc.body)
            .into_iter()
            .filter(|p| etext_at(ed, p.clone()) == "box")
            .count()
    };
    assert_eq!(live(&ed), 1, "the paste's alone: {}", saved(&ed));
    let mut accepted = cells();
    let clip = accepted.cut().expect("a clip");
    accepted.anchor = None;
    accepted.set_caret(Caret::at(vec![1], 0));
    accepted.paste(&clip);
    accepted.accept_all_revisions();
    assert_eq!(live(&accepted), 1, "{}", saved(&accepted));
    assert_eq!(boxed(&accepted), (String::new(), String::new()));
    ed.reject_all_revisions();
    assert_eq!(table(&saved(&ed)), table(&original));
    assert_eq!(live(&ed), 1, "the cell's box alone");
}

/// FIX r1 M1: the author's own pending insertion before two text boxes goes
/// outright, moving both boxes' inline indices; each box's text is still
/// recorded in its own box.
#[test]
fn a_tracked_clear_records_each_text_box_in_its_own_box_1471() {
    let host = vec![0, 0, 0, 0];
    let pending = format!("<w:ins w:id=\"1\" w:author=\"Ann\">{PIC}</w:ins>");
    let mut ed = table_of(&[
        format!("<w:p>{pending}{}{}</w:p>", text_box("one"), text_box("two")),
        format!("<w:p>{}</w:p>", t("c")),
    ]);
    track(&mut ed);
    ed.anchor = Some(Caret::at(host.clone(), 0));
    ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
    assert!(ed.delete_selection());
    assert!(!saved(&ed).contains("rId5"), "the own insertion went");
    for (nth, text) in [(0, "one"), (1, "two")] {
        let path = box_para(&ed, &host, nth);
        assert_eq!(etext_at(&ed, path.clone()), "", "{}", saved(&ed));
        assert_eq!(para_plain(&ed, path), text);
    }
}

const LINKED: &str = "<w:fldSimple w:instr=\" REF x \"><w:hyperlink w:anchor=\"x\">\
    <w:r><w:t>7</w:t></w:r></w:hyperlink></w:fldSimple>";

/// #1471: a tracked plain-text cut of cells copies their text but not the
/// text of what its delete keeps in place, an equation or a field a `w:del`
/// cannot hold (#1470), so a paste never doubles one; both stay. The clip
/// of a cut leaves both out too.
#[test]
fn a_tracked_plain_text_cut_of_cells_keeps_the_equation_1471() {
    let eqs = |ed: &Editor| saved(ed).matches("<m:oMath>").count();
    let mut ed = three_paras_and_a_table();
    track(&mut ed);
    select(&mut ed, vec![3, 0, 0, 0], 0, 0);
    ed.caret = Caret::at(vec![3, 0, 1, 0], 1);
    assert_eq!(ed.cut_text().as_deref(), Some("y\nx"));
    assert_eq!(eqs(&ed), 2);
    assert_eq!(cell_equations(&ed, vec![3, 0, 1, 0]), 1);

    let fields = |ed: &Editor| {
        resolve_para(&ed.doc.body, &[0, 0, 1, 0])
            .expect("a paragraph")
            .content
            .iter()
            .filter(|i| matches!(i, Inline::Field { .. }))
            .count()
    };
    let linked = || {
        let mut ed = table_of(&[
            format!("<w:p>{}</w:p>", t("y")),
            format!("<w:p>{}{LINKED}{}</w:p>", t("a"), t("b")),
        ]);
        track(&mut ed);
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 3);
        assert_eq!(fields(&ed), 1);
        ed
    };
    let mut ed = linked();
    assert_eq!(ed.cut_text().as_deref(), Some("y\nab"));
    assert_eq!(fields(&ed), 1);
    let mut ed = linked();
    let clip = ed.cut().expect("a clip");
    assert_eq!(clip.to_text(), "y\nab");
    assert!(
        !clip
            .paras
            .iter()
            .flatten()
            .any(|i| matches!(i, Inline::Field { .. })),
        "{clip:?}"
    );
    assert_eq!(fields(&ed), 1);
}

/// The `w:del` wrappers in the saved XML, each with its id.
fn deleted_objects(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<w:del ") {
        let end = rest[at..].find("</w:del>").map_or(rest.len(), |e| at + e);
        out.push(rest[at..end].to_string());
        rest = &rest[end..];
    }
    out
}

/// #1433: a tracked delete over a floating picture or an alignment tab
/// records each as a `w:del` with an id of its own (it kept them silently
/// before); reject keeps them, accept removes them, and a save and reload
/// keeps the records.
#[test]
fn a_tracked_delete_records_a_floating_picture_and_a_ptab_1433() {
    let mut ed = Editor::new(doc(&[format!(
        "{}{ANCHORED}{}{PTAB}{}",
        t("ab"),
        t("cd"),
        t("ef")
    )]));
    track(&mut ed);
    select(&mut ed, vec![0], 1, 5);
    assert!(ed.delete_selection());
    let xml = saved(&ed);
    let dels = deleted_objects(&xml);
    let with = |needle: &str| dels.iter().filter(|d| d.contains(needle)).count();
    assert_eq!(with("<w:drawing"), 1, "{xml}");
    assert_eq!(with("<w:ptab"), 1, "{xml}");
    assert!(dels.iter().all(|d| d.contains("w:author=\"Ann\"")), "{xml}");
    let ids: Vec<&str> = dels
        .iter()
        .filter(|d| d.contains("<w:drawing") || d.contains("<w:ptab"))
        .map(|d| {
            d.split("w:id=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "{xml}");

    // A save and reload keeps the records.
    let reloaded = parse_document_xml(&xml, &Relationships::default());
    assert_eq!(
        deleted_objects(&document_to_xml(&reloaded))
            .iter()
            .filter(|d| d.contains("<w:drawing") || d.contains("<w:ptab"))
            .count(),
        2
    );

    // Reject keeps both objects; accept removes them.
    let mut rejected = Editor::new(ed.doc.clone());
    rejected.reject_all_revisions();
    let after = saved(&rejected);
    assert!(
        after.contains("<w:drawing") && after.contains("<w:ptab"),
        "{after}"
    );
    assert!(!after.contains("<w:del "), "{after}");
    ed.accept_all_revisions();
    let after = saved(&ed);
    assert!(
        !after.contains("<w:drawing") && !after.contains("<w:ptab"),
        "{after}"
    );
}

/// #1433: tracked, across paragraphs, the middle paragraph's objects are
/// recorded too; edge objects are kept as untracked.
#[test]
fn a_tracked_delete_across_paragraphs_records_middle_objects_1433() {
    let mut ed = Editor::new(doc(&[
        format!("{}{PTAB}", t("ab")),
        format!("{ANCHORED}{}", t("cd")),
        t("ef"),
    ]));
    track(&mut ed);
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![2], 1);
    assert!(ed.delete_selection());
    let xml = saved(&ed);
    // The ptab is after "b" in the first paragraph: inside the range's
    // end of that paragraph, so it is recorded; so is the picture.
    assert_eq!(
        deleted_objects(&xml)
            .iter()
            .filter(|d| d.contains("<w:drawing") || d.contains("<w:ptab"))
            .count(),
        2,
        "{xml}"
    );
    ed.accept_all_revisions();
    let after = saved(&ed);
    assert!(
        !after.contains("<w:drawing") && !after.contains("<w:ptab"),
        "{after}"
    );
}

/// #1433 (r2-m4): a cut over three paragraphs keeps the zero-width objects
/// of the middle one, the clip not carrying them; a delete takes them.
#[test]
fn a_cut_keeps_the_objects_of_a_removed_middle_paragraph_1433() {
    let content = || vec![t("ab"), format!("{ANCHORED}{PTAB}{}", t("mid")), t("cd")];
    let at_end = |ed: &mut Editor| {
        ed.anchor = Some(Caret::at(vec![0], 1));
        ed.caret = Caret::at(vec![2], 1);
    };
    let mut ed = Editor::new(doc(&content()));
    at_end(&mut ed);
    let clip = ed.cut().expect("a clip");
    assert_eq!(ed.doc.body.len(), 1);
    assert_eq!(etext(&ed), "ad");
    let xml = saved(&ed);
    assert!(
        xml.contains("<w:drawing") && xml.contains("<w:ptab"),
        "{xml}"
    );
    assert_eq!(clip.paras.len(), 3);
    // Undo restores all of it.
    assert!(ed.undo());
    assert_eq!(ed.doc.body.len(), 3);
    assert_eq!(etext(&ed), "ab");
    // A plain delete takes the objects with the paragraph.
    let mut ed = Editor::new(doc(&content()));
    at_end(&mut ed);
    assert!(ed.delete_selection());
    let xml = saved(&ed);
    assert!(
        !xml.contains("<w:drawing") && !xml.contains("<w:ptab"),
        "{xml}"
    );
}

/// #1433: typing inside a document property control is seen at the caret,
/// and `set_bound_property` shows the new text in every control bound to the
/// property without an undo step of its own.
#[test]
fn typing_in_a_property_control_updates_the_others_1433() {
    use crate::sdt::{BoundProp, core_property_control};
    let control =
        |value: &str, id| core_property_control(BoundProp::Title, value, id, &RunProps::default());
    let body = vec![
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: [
                vec![Inline::Run(Run {
                    text: "T: ".into(),
                    props: RunProps::default(),
                })],
                control("Old", 1),
            ]
            .concat(),
        }),
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: [
                control("Old", 2),
                vec![Inline::Run(Run {
                    text: " end".into(),
                    props: RunProps::default(),
                })],
            ]
            .concat(),
        }),
    ];
    let mut ed = Editor::new(Document { body });
    // Nothing was edited yet; a caret move or a formatting change is no edit.
    ed.set_caret(Caret::at(vec![0], 6));
    assert_eq!(ed.take_bound_edits(), []);
    // Typing at the end of the control's text extends it.
    ed.insert_char('!');
    assert_eq!(ed.take_bound_edits(), [(BoundProp::Title, "Old!".into())]);
    assert_eq!(ed.take_bound_edits(), []);
    // The other control, in another paragraph, takes the text.
    assert!(ed.set_bound_property(BoundProp::Title, "Old!", PropSource::Here));
    let text = |ed: &Editor, i: usize| para(ed, i).plain_text();
    assert_eq!(text(&ed, 1), "Old! end");
    assert!(!ed.set_bound_property(BoundProp::Title, "Old!", PropSource::Here));
    assert_eq!(ed.take_bound_edits(), [], "the sync is not an edit");
    // Undoing the typing puts both back: the other control's change had no
    // step of its own. The host sees the old text as the edit.
    assert!(ed.undo());
    assert_eq!(text(&ed, 0), "T: Old");
    assert_eq!(text(&ed, 1), "Old end");
    assert_eq!(ed.take_bound_edits(), [(BoundProp::Title, "Old".into())]);
    // An emptied property shows the placeholder, flagged, and a value
    // clears the flag again.
    ed.set_bound_property(BoundProp::Title, "", PropSource::Here);
    assert_eq!(text(&ed, 1), "[Title] end");
    assert!(saved(&ed).matches("<w:showingPlcHdr/>").count() >= 1);
    ed.set_bound_property(BoundProp::Title, "Back", PropSource::Here);
    assert_eq!(text(&ed, 1), "Back end");
    assert!(!saved(&ed).contains("<w:showingPlcHdr/>"));
    // A control of another property is left alone.
    assert!(!ed.set_bound_property(BoundProp::Author, "Ann", PropSource::Here));
}

/// #1433: a control inserted, or a change that leaves every control's text
/// alone, is not an edit of a property.
#[test]
fn only_a_changed_control_text_is_a_bound_edit_1433() {
    use crate::sdt::{BoundProp, core_property_control};
    let control = core_property_control(BoundProp::Title, "Old", 1, &RunProps::default());
    let mut ed = Editor::new(Document {
        body: vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: control,
        })],
    });
    // Bold over the control's text: no edit.
    ed.anchor = Some(Caret::at(vec![0], 0));
    ed.caret = Caret::at(vec![0], 3);
    ed.toggle_bold();
    assert_eq!(ed.take_bound_edits(), []);
    // A second control pasted in: the count changed, not an edit.
    ed.set_caret(Caret::at(vec![0], 3));
    let other = core_property_control(BoundProp::Title, "Old", 2, &RunProps::default());
    ed.insert_object(other);
    assert_eq!(ed.take_bound_edits(), []);
}

fn first_content(ed: &mut Editor) -> &mut Vec<Inline> {
    match &mut ed.doc.body[0] {
        Block::Paragraph(p) => &mut p.content,
        _ => panic!(),
    }
}

/// #1433: controls are paired by their content-control id. Two properties
/// changed in one step both report; a swap, or a control replaced by another,
/// reports nothing; a count change still reports a surviving control's text.
#[test]
fn bound_edits_pair_controls_by_id_1433() {
    use crate::sdt::{BoundProp, core_property_control, scan_paragraph, set_control_text};
    let two = || {
        let mut content = core_property_control(BoundProp::Title, "T", 1, &RunProps::default());
        content.extend(core_property_control(
            BoundProp::Author,
            "A",
            2,
            &RunProps::default(),
        ));
        Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content,
            })],
        })
    };
    // Both texts change in one step: one edit per property.
    let mut ed = two();
    let c = first_content(&mut ed);
    for k in (0..2).rev() {
        let found = scan_paragraph(c);
        set_control_text(c, &found[k], if k == 0 { "T2" } else { "A2" });
    }
    assert_eq!(
        ed.take_bound_edits(),
        [
            (BoundProp::Title, "T2".into()),
            (BoundProp::Author, "A2".into())
        ]
    );
    // The two swapped: no edit.
    let mut ed = two();
    let c = first_content(&mut ed);
    let (title, author) = c.split_at(3);
    *c = [author, title].concat();
    assert_eq!(ed.take_bound_edits(), []);
    // One replaced by another control of the same count and another id.
    let mut ed = two();
    let c = first_content(&mut ed);
    c.splice(
        0..3,
        core_property_control(BoundProp::Title, "Other", 9, &RunProps::default()),
    );
    assert_eq!(ed.take_bound_edits(), []);
    // One removed while the other's text changed: the survivor reports.
    let mut ed = two();
    let c = first_content(&mut ed);
    let found = scan_paragraph(c);
    set_control_text(c, &found[1], "A3");
    c.drain(0..3);
    assert_eq!(ed.take_bound_edits(), [(BoundProp::Author, "A3".into())]);
}

/// #1433: a property set from the other editor is an undo step here, named
/// for it; one run of the other editor's typing is one step, a new step of
/// the other editor is a new one, and after an undo a sync pushes a fresh
/// step, clearing the redo it would otherwise leave behind.
#[test]
fn a_property_from_the_other_editor_is_one_step_per_source_step_1433() {
    use crate::sdt::{BoundProp, core_property_control};
    let control = |id| core_property_control(BoundProp::Title, "Old", id, &RunProps::default());
    let mut ed = Editor::new(Document {
        body: vec![
            Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: control(1),
            }),
            Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: control(2),
            }),
        ],
    });
    let from = |step| PropSource::Other {
        newest_step: Some(step),
    };
    let n0 = ed.undo_names().len();
    assert!(ed.set_bound_property(BoundProp::Title, "A", from(10)));
    assert!(ed.set_bound_property(BoundProp::Title, "AB", from(10)));
    assert_eq!(ed.undo_names().len(), n0 + 1);
    assert_eq!(ed.undo_names()[0], "Title");
    // The other editor's next step is a step of ours.
    assert!(ed.set_bound_property(BoundProp::Title, "ABC", from(11)));
    assert_eq!(ed.undo_names().len(), n0 + 2);
    // An undo brings the earlier text back; a sync then is a fresh step and
    // leaves nothing to redo.
    assert!(ed.undo());
    assert_eq!(para(&ed, 0).plain_text(), "AB");
    assert!(ed.can_redo());
    assert!(ed.set_bound_property(BoundProp::Title, "Z", from(11)));
    assert!(!ed.can_redo());
    assert_eq!(ed.undo_names().len(), n0 + 2);
    // The sync step is the newest with a redo waiting (an unrelated edit
    // undone): a sync from the same source step is a fresh step, and the
    // redo is gone.
    let mut ed2 = Editor::new(ed.doc.clone());
    assert!(ed2.set_bound_property(BoundProp::Title, "Q", from(20)));
    let steps = ed2.undo_names().len();
    ed2.set_caret(Caret::at(vec![1], 0));
    ed2.insert_char('x');
    assert!(ed2.undo());
    assert!(ed2.can_redo());
    assert!(ed2.set_bound_property(BoundProp::Title, "QR", from(20)));
    assert!(!ed2.can_redo());
    assert_eq!(ed2.undo_names().len(), steps + 1);
    // A sync that changes nothing pushes nothing.
    assert!(!ed.set_bound_property(BoundProp::Title, "Z", from(12)));
    assert_eq!(ed.undo_names().len(), n0 + 2);
}

/// #1433: all the properties one step of the other editor changes are one
/// step here, and a later keystroke of that step merges into it.
#[test]
fn two_properties_from_one_source_step_are_one_step_1433() {
    use crate::sdt::{BoundProp, core_property_control};
    let mut content = core_property_control(BoundProp::Title, "T", 1, &RunProps::default());
    content.extend(core_property_control(
        BoundProp::Company,
        "C",
        2,
        &RunProps::default(),
    ));
    let mut ed = Editor::new(Document {
        body: vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content,
        })],
    });
    let from = PropSource::Other {
        newest_step: Some(7),
    };
    let n0 = ed.undo_names().len();
    assert!(ed.set_bound_property(BoundProp::Title, "T2", from));
    assert!(ed.set_bound_property(BoundProp::Company, "C2", from));
    assert!(ed.set_bound_property(BoundProp::Title, "T23", from));
    assert_eq!(ed.undo_names().len(), n0 + 1);
    assert_eq!(ed.undo_names()[0], "Title");
}

/// #1364: File > Info's Title, Tags and Author are one undo step each, shown in
/// the bound controls inside that step.
#[test]
fn a_core_field_is_an_undo_step_that_the_controls_follow_1364() {
    use crate::sdt::{BoundProp, core_property_control};
    let body = vec![Block::Paragraph(Paragraph {
        props: ParProps::default(),
        content: core_property_control(BoundProp::Title, "Old", 1, &RunProps::default()),
    })];
    let mut ed = Editor::new(Document { body });
    ed.reseed_core_fields(CoreFields {
        title: "Old".into(),
        ..CoreFields::default()
    });
    let text = |ed: &Editor| para(ed, 0).plain_text();
    assert!(!ed.set_core_field(BoundProp::Title, "Old"), "unchanged");
    assert!(
        !ed.set_core_field(BoundProp::Subject, "x"),
        "not an Info field"
    );
    assert_eq!(ed.undo_serial(), None, "no step for either");
    assert!(ed.set_core_field(BoundProp::Title, "New"));
    assert_eq!(ed.core_fields().title, "New");
    assert_eq!(text(&ed), "New");
    assert_eq!(ed.take_bound_edits(), [], "the host's own write is no edit");
    assert!(ed.set_core_field(BoundProp::Keywords, "a, b"));
    assert_eq!(ed.core_fields().tags, "a, b");
    assert!(ed.undo());
    assert_eq!(ed.core_fields().tags, "");
    assert_eq!(ed.core_fields().title, "New");
    assert!(ed.undo());
    assert_eq!(ed.core_fields().title, "Old");
    assert_eq!(text(&ed), "Old");
    assert!(!ed.undo());
    assert!(ed.redo() && ed.redo());
    assert_eq!(ed.core_fields().title, "New");
    assert_eq!(ed.core_fields().tags, "a, b");
    assert_eq!(text(&ed), "New");
    // The host sees what an edit, undo or redo changed, and writes it.
    assert_eq!(
        ed.core_unsynced(),
        [
            (BoundProp::Title, "New".to_string()),
            (BoundProp::Keywords, "a, b".to_string())
        ]
    );
    ed.core_synced();
    assert_eq!(ed.core_unsynced(), []);
    assert!(ed.undo());
    assert_eq!(ed.core_unsynced(), [(BoundProp::Keywords, String::new())]);
    // A control typed in is noted without a step of its own, and is synced.
    ed.note_core_field(BoundProp::Author, "Ann");
    assert_eq!(ed.core_fields().author, "Ann");
    ed.note_core_field(BoundProp::Subject, "ignored");
    // Reseeding changes every step: an undo cannot bring back the old file.
    ed.reseed_core_fields(CoreFields::default());
    assert!(ed.undo());
    assert_eq!(ed.core_fields(), &CoreFields::default());
    assert_eq!(ed.core_unsynced(), []);
}

/// #1364: a property typed in the other editor (a header) is one body undo
/// step whose undo puts the controls and the core field back together.
#[test]
fn a_property_synced_from_a_header_undoes_with_its_controls_1364() {
    use crate::sdt::{BoundProp, core_property_control};
    let body = vec![Block::Paragraph(Paragraph {
        props: ParProps::default(),
        content: core_property_control(BoundProp::Title, "Old", 1, &RunProps::default()),
    })];
    let mut ed = Editor::new(Document { body });
    ed.reseed_core_fields(CoreFields {
        title: "Old".into(),
        ..CoreFields::default()
    });
    assert!(ed.set_core_field(BoundProp::Title, "New"));
    ed.core_synced();
    let from = PropSource::Other {
        newest_step: Some(9_999_999),
    };
    assert!(ed.file_property_changed(BoundProp::Title, "Hdr", from));
    assert_eq!(ed.core_fields().title, "Hdr");
    assert_eq!(ed.core_unsynced(), []);
    assert!(ed.undo());
    assert_eq!(para(&ed, 0).plain_text(), "New");
    assert_eq!(
        ed.core_fields().title,
        "New",
        "the field undoes with the controls"
    );
    assert_eq!(ed.core_unsynced(), [(BoundProp::Title, "New".to_string())]);
}

/// #1364: with no bound control in the body, a property synced from the
/// header has no body step, so no body undo or redo may put the old value
/// back.
#[test]
fn a_header_property_with_no_body_control_survives_body_undo_1364() {
    use crate::sdt::BoundProp;
    let mut ed = Editor::new(doc(&[t("body")]));
    ed.reseed_core_fields(CoreFields {
        title: "Old".into(),
        ..CoreFields::default()
    });
    ed.set_caret(Caret::at(vec![0], 4));
    ed.insert_char('!');
    let from = PropSource::Other {
        newest_step: Some(9_999_999),
    };
    assert!(!ed.file_property_changed(BoundProp::Title, "New", from));
    assert_eq!(ed.core_fields().title, "New");
    assert_eq!(ed.core_unsynced(), []);
    assert!(ed.undo());
    assert_eq!(para(&ed, 0).plain_text(), "body");
    assert_eq!(ed.core_fields().title, "New");
    assert_eq!(ed.core_unsynced(), []);
    assert!(ed.redo());
    assert_eq!(ed.core_fields().title, "New");
    assert_eq!(ed.core_unsynced(), []);
}

/// The raw inlines of paragraph 0, verbatim.
fn raws(ed: &Editor) -> Vec<String> {
    para(ed, 0)
        .content
        .iter()
        .filter_map(|i| match i {
            Inline::Raw(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

const PTAB_EL: &str = "<w:ptab w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"none\"/>";

/// #1463: a run holding a field marker and an alignment tab (an orphan
/// marker here, so the run stays raw) loses only the tab to a delete over
/// it: the marker stays in its run, as in Word, so no field is left
/// unbalanced. Undo brings the run back as it was.
#[test]
fn a_delete_over_a_ptab_keeps_a_field_marker_in_its_run_1463() {
    let end = "<w:fldChar w:fldCharType=\"end\"/>";
    let begin = "<w:fldChar w:fldCharType=\"begin\"/>";
    let rpr = "<w:rPr><w:b/></w:rPr>";
    for (run, kept) in [
        (
            format!("<w:r>{end}{PTAB_EL}</w:r>"),
            format!("<w:r>{end}</w:r>"),
        ),
        (
            format!("<w:r>{PTAB_EL}{begin}</w:r>"),
            format!("<w:r>{begin}</w:r>"),
        ),
        (
            format!("<w:r w:rsidR=\"00A1\">{rpr}{end}{PTAB_EL}</w:r>"),
            format!("<w:r w:rsidR=\"00A1\">{rpr}{end}</w:r>"),
        ),
    ] {
        let mut ed = ed_with(&run);
        assert_eq!(
            raws(&ed),
            std::slice::from_ref(&run),
            "loaded as one raw run"
        );
        select(&mut ed, vec![0], 1, 3);
        assert!(ed.delete_selection());
        assert_eq!(raws(&ed), std::slice::from_ref(&kept), "{run}");
        assert_eq!(etext(&ed), "ad");
        assert!(ed.undo());
        assert_eq!(raws(&ed), std::slice::from_ref(&run), "undo: {run}");
        assert_eq!(etext(&ed), "abcd");
    }
}

/// #1463: tracked, the marker's run stays undeleted and the alignment tab
/// is recorded alone in a `w:del`, in a run with the original properties
/// (a `w:rPrChange` with an `w:rPr` of its own in them included). Accept
/// removes the tab, reject keeps it, and either way the field's end stays.
#[test]
fn a_tracked_delete_over_a_ptab_keeps_a_field_marker_in_its_run_1463() {
    let end = "<w:fldChar w:fldCharType=\"end\"/>";
    let rpr = "<w:rPr><w:b/><w:rPrChange w:id=\"9\" w:author=\"Bo\" \
        w:date=\"2026-01-01T00:00:00Z\"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr>";
    let count = |xml: &str, needle: &str| xml.matches(needle).count();
    for tab_first in [false, true] {
        let run = if tab_first {
            format!("<w:r>{rpr}{PTAB_EL}{end}</w:r>")
        } else {
            format!("<w:r>{rpr}{end}{PTAB_EL}</w:r>")
        };
        let mut ed = ed_with(&run);
        let before = saved(&ed);
        track(&mut ed);
        select(&mut ed, vec![0], 1, 3);
        assert!(ed.delete_selection());
        let xml = saved(&ed);
        assert_eq!(count(&xml, "fldCharType=\"end\""), 1, "{xml}");
        assert_eq!(count(&xml, "<w:ptab"), 1, "{xml}");
        let dels = deleted_objects(&xml);
        let tab_dels: Vec<&String> = dels.iter().filter(|d| d.contains("<w:ptab")).collect();
        assert_eq!(tab_dels.len(), 1, "{xml}");
        assert!(tab_dels[0].contains(rpr), "the run's properties: {xml}");
        assert!(dels.iter().all(|d| !d.contains("<w:fldChar")), "{xml}");
        // The tab's record sits on the side of the marker the tab was on.
        let (tab_at, end_at) = (xml.find("<w:ptab").unwrap(), xml.find(end).unwrap());
        assert_eq!(tab_at < end_at, tab_first, "{xml}");

        let mut rejected = Editor::new(ed.doc.clone());
        rejected.reject_all_revisions();
        let after = saved(&rejected);
        assert_eq!(count(&after, "fldCharType=\"end\""), 1, "{after}");
        assert_eq!(count(&after, "<w:ptab"), 1, "{after}");
        let mut accepted = Editor::new(ed.doc.clone());
        accepted.accept_all_revisions();
        let after = saved(&accepted);
        assert_eq!(count(&after, "fldCharType=\"end\""), 1, "{after}");
        assert_eq!(count(&after, "<w:ptab"), 0, "{after}");

        assert!(ed.undo());
        assert_eq!(saved(&ed), before, "undo restores the run");
    }
}

/// The field markers (by type) and alignment tabs of `xml`, in order.
fn markers_and_tabs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(rel) = xml[at..].find('<') {
        at += rel;
        let rest = &xml[at..];
        if rest.starts_with("<w:ptab") {
            out.push("ptab".to_string());
        } else if let Some(kind) = rest.strip_prefix("<w:fldChar w:fldCharType=\"") {
            out.push(kind.split('"').next().unwrap().to_string());
        }
        at += 1;
    }
    out
}

/// #1463 FIX r1 m1: tracked, tabs mixed with markers are recorded where each
/// is, so reject puts every tab back in its place among the markers and
/// accept leaves the markers in their order.
#[test]
fn a_tracked_delete_keeps_mixed_tabs_and_markers_in_order_1463() {
    let fld = |kind: &str| format!("<w:fldChar w:fldCharType=\"{kind}\"/>");
    let rpr = "<w:rPr><w:b/></w:rPr>";
    for run in [
        format!("<w:r>{rpr}{PTAB_EL}{}{PTAB_EL}</w:r>", fld("end")),
        format!("<w:r>{rpr}{}{PTAB_EL}{}</w:r>", fld("separate"), fld("end")),
        format!("<w:r>{rpr}{}{PTAB_EL}{}</w:r>", fld("end"), fld("begin")),
    ] {
        let mut ed = ed_with(&run);
        assert_eq!(
            raws(&ed),
            std::slice::from_ref(&run),
            "loaded as one raw run"
        );
        let order = markers_and_tabs(&saved(&ed));
        track(&mut ed);
        select(&mut ed, vec![0], 1, 3);
        assert!(ed.delete_selection());
        let xml = saved(&ed);
        assert_eq!(markers_and_tabs(&xml), order, "{xml}");
        let dels = deleted_objects(&xml);
        assert!(dels.iter().all(|d| !d.contains("<w:fldChar")), "{xml}");
        let tab_dels: Vec<&String> = dels.iter().filter(|d| d.contains("<w:ptab")).collect();
        assert!(!tab_dels.is_empty(), "{xml}");
        assert!(tab_dels.iter().all(|d| d.contains(rpr)), "{xml}");

        let mut rejected = Editor::new(ed.doc.clone());
        rejected.reject_all_revisions();
        let after = saved(&rejected);
        assert_eq!(markers_and_tabs(&after), order, "reject: {after}");
        let mut accepted = Editor::new(ed.doc.clone());
        accepted.accept_all_revisions();
        let after = saved(&accepted);
        let markers: Vec<String> = order.iter().filter(|m| *m != "ptab").cloned().collect();
        assert_eq!(markers_and_tabs(&after), markers, "accept: {after}");

        // Untracked, the run keeps its markers alone, in order.
        let mut plain = ed_with(&run);
        select(&mut plain, vec![0], 1, 3);
        assert!(plain.delete_selection());
        assert_eq!(markers_and_tabs(&saved(&plain)), markers, "untracked");
    }
}
