//! #1625: a field inserted while tracking (Insert Field, Date & Time, a
//! paste) is recorded as a `w:ins` of its own, as a picture is (#1242):
//! zero-width until accepted. Typing right after it goes after it, and its
//! own author's Backspace or Delete next to it removes it outright, so a
//! tracked delete never turns it into a `w:del` that Reject brings back.

use docxcore::editor::{Caret, Clip, Editor, TrackAuthor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, RevisionKind, RunProps};
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

const COMPLEX: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
    <w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
    <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>7</w:t></w:r>\
    <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
const LINKED: &str = "<w:fldSimple w:instr=\" REF x \"><w:hyperlink w:anchor=\"x\">\
    <w:r><w:t>7</w:t></w:r></w:hyperlink></w:fldSimple>";
const PIC: &str = "<w:r><w:drawing><wp:inline><wp:extent cx=\"952500\" cy=\"952500\"/>\
    <a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed=\"rId5\"/></pic:blipFill>\
    </pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r>";

fn track(ed: &mut Editor, author: &str) {
    ed.set_track_changes(Some(TrackAuthor {
        author: author.into(),
        clock,
    }));
}

/// An editor on `abcd`, tracking as Ada.
fn editor() -> Editor {
    let mut ed = Editor::new(parse("<w:p><w:r><w:t>abcd</w:t></w:r></w:p>"));
    track(&mut ed, "Ada");
    ed
}

/// A `w:fldSimple` as the suite's Insert Field and Date & Time build one.
fn simple_field() -> Inline {
    docxcore::field::fld_simple(" PAGE ", "7", &RunProps::default())
}

/// The field one paragraph of `inner` loads into.
fn loaded(inner: &str) -> Inline {
    let doc = parse(&format!("<w:p>{inner}</w:p>"));
    let Block::Paragraph(p) = &doc.body[0] else {
        panic!("paragraph")
    };
    p.content[0].clone()
}

fn complex_field() -> Inline {
    loaded(COMPLEX)
}

fn picture() -> Inline {
    loaded(PIC)
}

/// `inline` pasted at offset `at` of the first paragraph.
fn insert_at(ed: &mut Editor, at: usize, inline: Inline) {
    ed.set_caret(Caret::at(vec![0], at));
    ed.paste(&Clip::new(vec![vec![inline]]));
}

fn content(ed: &Editor) -> &[Inline] {
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    &p.content
}

/// The first paragraph as a list: text runs, `F` for a live field,
/// `ins(F)`/`ins(P)` for a pending field or picture insertion, `del(..)` for
/// a deletion.
fn shape(ed: &Editor) -> Vec<String> {
    fn one(i: &Inline) -> String {
        match i {
            Inline::Run(r) => r.text.clone(),
            Inline::Field { .. } => "F".into(),
            Inline::Raw(_) => "P".into(),
            Inline::Revision { kind, content, .. } => {
                let inner: Vec<String> = content.iter().map(one).collect();
                let tag = match kind {
                    RevisionKind::Insert => "ins",
                    RevisionKind::Delete => "del",
                };
                format!("{tag}({})", inner.join(""))
            }
            other => format!("{other:?}"),
        }
    }
    content(ed).iter().map(one).collect()
}

fn xml(ed: &Editor) -> String {
    document_to_xml(&ed.doc)
}

/// The text inside each `w:ins` of `xml`.
fn insertions(xml: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<w:ins ") {
        let wrapper = &rest[at..at + rest[at..].find("</w:ins>").unwrap()];
        out.push(wrapper);
        rest = &rest[at + wrapper.len()..];
    }
    out
}

fn ids(xml: &str) -> Vec<&str> {
    xml.match_indices("<w:ins ")
        .map(|(at, _)| {
            let id = &xml[at..][xml[at..].find("w:id=\"").unwrap() + 6..];
            &id[..id.find('"').unwrap()]
        })
        .collect()
}

fn reloaded(ed: &Editor) -> Editor {
    let saved = xml(ed);
    let body = &saved[saved.find("<w:body>").unwrap() + 8..saved.find("</w:body>").unwrap()];
    Editor::new(parse(body))
}

fn live_fields(ed: &Editor) -> usize {
    content(ed)
        .iter()
        .filter(|i| matches!(i, Inline::Field { .. }))
        .count()
}

fn text(ed: &Editor) -> String {
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("paragraph")
    };
    p.plain_text()
}

#[test]
fn a_field_inserted_while_tracking_is_a_w_ins_of_its_own() {
    for field in [simple_field(), complex_field()] {
        let mut ed = editor();
        insert_at(&mut ed, 2, field);
        assert_eq!(shape(&ed), ["ab", "ins(F)", "cd"]);
        assert_eq!(ed.caret.offset, 2, "a pending insertion takes no offset");
        let Inline::Revision {
            content: inner,
            content_changed,
            ..
        } = &content(&ed)[1]
        else {
            unreachable!()
        };
        assert!(content_changed);
        let Inline::Field { raw, text } = &inner[0] else {
            unreachable!()
        };
        assert!(!raw.contains("<w:fldSimple"), "{raw}");
        assert_eq!(text, "7");

        let x = xml(&ed);
        let ins = insertions(&x);
        assert_eq!(ins.len(), 1, "{x}");
        assert!(ins[0].contains("fldCharType=\"begin\""), "{x}");
        assert!(!ins[0].contains("<w:fldSimple"), "{x}");
    }
}

#[test]
fn text_and_field_pasted_together_take_distinct_ids() {
    let mut ed = editor();
    let run = Inline::Run(docxcore::model::Run {
        text: "xy".into(),
        props: RunProps::default(),
    });
    ed.set_caret(Caret::at(vec![0], 2));
    ed.paste(&Clip::new(vec![vec![run, simple_field(), picture()]]));
    let x = xml(&ed);
    let ids = ids(&x);
    assert_eq!(ids.len(), 3, "{x}");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "{x}");
}

#[test]
fn reject_removes_the_inserted_field_and_accept_keeps_it() {
    for field in [simple_field(), complex_field()] {
        let mut ed = editor();
        insert_at(&mut ed, 2, field);
        for doc in [ed.doc.clone(), reloaded(&ed).doc] {
            let mut rejected = Editor::new(doc.clone());
            rejected.reject_all_revisions();
            assert_eq!(live_fields(&rejected), 0, "{}", xml(&rejected));
            assert_eq!(text(&rejected), "abcd");
            assert!(!xml(&rejected).contains("instrText"));

            let mut accepted = Editor::new(doc);
            accepted.accept_all_revisions();
            assert_eq!(shape(&accepted), ["ab", "F", "cd"]);
            assert_eq!(text(&accepted), "ab7cd");
            assert!(!xml(&accepted).contains("<w:ins "));
        }
    }
}

#[test]
fn an_untracked_paste_of_a_field_is_unchanged() {
    let mut ed = editor();
    ed.set_track_changes(None);
    insert_at(&mut ed, 2, simple_field());
    assert_eq!(shape(&ed), ["ab", "F", "cd"]);
    assert_eq!(ed.caret.offset, 3);
    assert!(!xml(&ed).contains("<w:ins "));
}

#[test]
fn a_field_a_w_ins_cannot_hold_goes_in_live() {
    let mut ed = editor();
    insert_at(&mut ed, 2, loaded(LINKED));
    assert_eq!(shape(&ed), ["ab", "F", "cd"]);
    let x = xml(&ed);
    assert!(!x.contains("<w:ins "), "{x}");
}

#[test]
fn backspace_removes_ones_own_pending_insertion() {
    for object in [simple_field(), complex_field(), picture()] {
        let pending = if matches!(object, Inline::Raw(_)) {
            "ins(P)"
        } else {
            "ins(F)"
        };
        let mut ed = editor();
        insert_at(&mut ed, 2, object.clone());
        ed.backspace();
        assert_eq!(shape(&ed).concat(), "abcd");
        assert_eq!(ed.caret.offset, 2);
        let x = xml(&ed);
        assert!(!x.contains("<w:del ") && !x.contains("<w:ins "), "{x}");
        assert!(ed.undo(), "one step");
        assert_eq!(shape(&ed), ["ab", pending, "cd"]);
        assert!(ed.redo());
        // The next press deletes text as before.
        ed.backspace();
        assert_eq!(shape(&ed), ["a", "del(b)", "cd"]);
    }
}

/// The caret is after a pending insertion at its offset (typing there goes
/// after it), so Delete acts on what follows, as before #1625.
#[test]
fn delete_leaves_a_pending_insertion_and_acts_on_what_follows() {
    for object in [simple_field(), picture()] {
        let pending = if matches!(object, Inline::Raw(_)) {
            "ins(P)"
        } else {
            "ins(F)"
        };
        let mut ed = editor();
        insert_at(&mut ed, 2, object.clone());
        ed.delete_forward();
        assert_eq!(shape(&ed), ["ab", pending, "del(c)", "d"]);

        // At a paragraph's end it joins the next paragraph.
        let mut ed = Editor::new(parse(
            "<w:p><w:r><w:t>abcd</w:t></w:r></w:p><w:p><w:r><w:t>ef</w:t></w:r></w:p>",
        ));
        track(&mut ed, "Ada");
        insert_at(&mut ed, 4, object.clone());
        ed.delete_forward();
        assert_eq!(ed.doc.body.len(), 1, "{}", xml(&ed));
        assert_eq!(shape(&ed), ["abcd", pending, "ef"]);
    }
}

#[test]
fn another_authors_pending_insertion_is_left_alone() {
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    track(&mut ed, "Bo");
    ed.backspace();
    assert_eq!(shape(&ed), ["a", "del(b)", "ins(F)", "cd"]);
    // And a loaded one, as Word writes it.
    let mut ed = Editor::new(parse(&format!(
        "<w:p><w:r><w:t>ab</w:t></w:r><w:ins w:id=\"1\" w:author=\"Ada\">{COMPLEX}</w:ins>\
         <w:r><w:t>cd</w:t></w:r></w:p>"
    )));
    track(&mut ed, "Bo");
    ed.set_caret(Caret::at(vec![0], 2));
    ed.backspace();
    assert_eq!(shape(&ed), ["a", "del(b)", "ins(F)", "cd"]);
    // Ada's own Backspace removes it.
    track(&mut ed, "Ada");
    ed.set_caret(Caret::at(vec![0], 1));
    ed.backspace();
    assert_eq!(shape(&ed), ["a", "del(b)", "cd"]);
}

#[test]
fn untracked_backspace_is_unchanged_next_to_a_pending_insertion() {
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    ed.set_track_changes(None);
    ed.backspace();
    assert_eq!(shape(&ed), ["a", "ins(F)", "cd"]);
}

#[test]
fn a_pending_insertion_inside_a_link_is_removed() {
    for (link, left) in [
        // After the link's text, and as all of it (the emptied link goes).
        ("<w:r><w:t>L</w:t></w:r>{INS}", "abLcd"),
        ("{INS}", "abcd"),
    ] {
        let ins = format!("<w:ins w:id=\"1\" w:author=\"Ada\">{COMPLEX}</w:ins>");
        let link = link.replace("{INS}", &ins);
        let mut ed = Editor::new(parse(&format!(
            "<w:p><w:r><w:t>ab</w:t></w:r><w:hyperlink w:anchor=\"x\">{link}</w:hyperlink>\
             <w:r><w:t>cd</w:t></w:r></w:p>"
        )));
        track(&mut ed, "Ada");
        let at = left.len() - 2;
        ed.set_caret(Caret::at(vec![0], at));
        ed.backspace();
        let x = xml(&ed);
        assert!(!x.contains("<w:ins ") && !x.contains("<w:del "), "{x}");
        assert!(!x.contains("instrText"), "{x}");
        assert_eq!(text(&ed), left);
        assert_eq!(ed.caret.offset, at);
        if left == "abcd" {
            assert!(!x.contains("<w:hyperlink"), "{x}");
        }
    }
}

#[test]
fn text_typed_after_a_pending_insertion_follows_it() {
    for object in [simple_field(), picture()] {
        // Mid-paragraph and at its end.
        for (at, want) in [(2, ["ab", "ins", "x", "cd"]), (4, ["abcd", "ins", "x", ""])] {
            let mut ed = editor();
            insert_at(&mut ed, at, object.clone());
            ed.insert_char('x');
            assert_eq!(ed.caret.offset, at + 1);
            let got: Vec<String> = shape(&ed)
                .into_iter()
                .map(|s| {
                    if s.starts_with("ins(") {
                        "ins".to_string()
                    } else {
                        s
                    }
                })
                .collect();
            let want: Vec<&str> = want.into_iter().filter(|s| !s.is_empty()).collect();
            assert_eq!(got, want);
            assert_eq!(text(&ed).replace(['7', '\u{fffc}'], ""), {
                let mut t = "abcd".to_string();
                t.insert(at, 'x');
                t
            });
        }
    }
}

/// Delete or cut over `b[F]c` (offsets 1..3).
fn delete_over_it(cut: bool) -> Editor {
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 3);
    if cut {
        assert!(ed.cut().is_some());
    } else {
        assert!(ed.delete_selection());
    }
    ed
}

#[test]
fn a_selection_delete_over_ones_own_pending_insertion_removes_it() {
    for cut in [false, true] {
        let ed = delete_over_it(cut);
        assert_eq!(shape(&ed), ["a", "del(bc)", "d"], "cut {cut}");
        let x = xml(&ed);
        assert!(!x.contains("<w:ins ") && !x.contains("instrText"), "{x}");
        let mut rejected = Editor::new(ed.doc.clone());
        rejected.reject_all_revisions();
        assert_eq!(text(&rejected), "abcd");
        assert_eq!(live_fields(&rejected), 0);
        let mut accepted = ed;
        accepted.accept_all_revisions();
        assert_eq!(text(&accepted), "ad");
        assert_eq!(live_fields(&accepted), 0);
    }
}

#[test]
fn a_cut_over_ones_own_pending_insertion_moves_it_once() {
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 3);
    let clip = ed.cut().expect("a clip");
    assert_eq!(clip.paras.len(), 1);
    assert_eq!(
        clip.paras[0]
            .iter()
            .filter(|i| matches!(i, Inline::Field { .. }))
            .count(),
        1,
        "the clip carries the field"
    );
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![0], 4));
    ed.paste(&clip);
    let x = xml(&ed);
    assert_eq!(
        x.matches("<w:instrText").count(),
        1,
        "one field, not two: {x}"
    );
    let mut accepted = Editor::new(ed.doc.clone());
    accepted.accept_all_revisions();
    assert_eq!(live_fields(&accepted), 1, "{}", xml(&accepted));
    let mut rejected = Editor::new(ed.doc.clone());
    rejected.reject_all_revisions();
    assert_eq!(text(&rejected), "abcd");
    assert_eq!(live_fields(&rejected), 0);
}

#[test]
fn a_pending_insertion_at_a_selections_edges() {
    // At `hi` it is covered (the caret there is after it): removed.
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 2);
    assert!(ed.delete_selection());
    assert_eq!(shape(&ed), ["a", "del(b)", "cd"]);
    // At `lo` it is before the selection: kept.
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    ed.anchor = Some(Caret::at(vec![0], 2));
    ed.caret = Caret::at(vec![0], 3);
    assert!(ed.delete_selection());
    assert_eq!(shape(&ed), ["ab", "ins(F)", "del(c)", "d"]);
}

#[test]
fn a_selection_across_paragraphs_removes_ones_own_pending_insertions() {
    let mut ed = Editor::new(parse(
        "<w:p><w:r><w:t>abcd</w:t></w:r></w:p><w:p><w:r><w:t>ef</w:t></w:r></w:p>",
    ));
    track(&mut ed, "Ada");
    // One at the first paragraph's end, one at the second's start.
    insert_at(&mut ed, 4, simple_field());
    ed.set_caret(Caret::at(vec![1], 0));
    ed.paste(&Clip::new(vec![vec![simple_field()]]));
    assert_eq!(insertions(&xml(&ed)).len(), 2, "{}", xml(&ed));
    ed.anchor = Some(Caret::at(vec![0], 3));
    ed.caret = Caret::at(vec![1], 1);
    assert!(ed.delete_selection());
    let x = xml(&ed);
    assert!(!x.contains("instrText"), "{x}");
}

#[test]
fn another_authors_pending_insertion_survives_a_selection_delete() {
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    track(&mut ed, "Bo");
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 3);
    assert!(ed.delete_selection());
    assert_eq!(shape(&ed).iter().filter(|s| *s == "ins(F)").count(), 1);
}

/// Text typed after a pending insertion, right after the author's own typed
/// text, is an insertion of its own: no `w:id` is shared by two stretches.
#[test]
fn text_typed_after_a_pending_insertion_takes_an_id_of_its_own() {
    let mut ed = Editor::new(parse("<w:p/>"));
    track(&mut ed, "Ada");
    ed.set_caret(Caret::at(vec![0], 0));
    ed.insert_str("ab");
    ed.paste(&Clip::new(vec![vec![simple_field()]]));
    ed.insert_char('x');
    let x = xml(&ed);
    let ids = ids(&x);
    assert_eq!(ids.len(), 3, "{x}");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "{x}");
    assert_eq!(reloaded(&ed).doc.revisions().len(), 3, "{x}");
}

#[test]
fn a_merge_field_inserted_while_tracking_still_previews() {
    use docxcore::merge::Recipients;
    use docxcore::merge::fields::{FieldMap, merge_field};
    use docxcore::merge::preview::{MergePreview, apply_preview};
    let mut ed = editor();
    insert_at(&mut ed, 2, merge_field("First", &RunProps::default()));
    assert_eq!(shape(&ed), ["ab", "ins(F)", "cd"]);
    let recipients = Recipients::parse_csv(b"First\nJane\n").unwrap();
    let map = FieldMap::auto(&recipients);
    apply_preview(
        &mut ed.doc,
        Some(&MergePreview {
            recipients,
            map,
            row: 0,
        }),
    );
    let Inline::Revision { content: inner, .. } = &content(&ed)[1] else {
        unreachable!()
    };
    let Inline::Field { text, .. } = &inner[0] else {
        unreachable!()
    };
    assert_eq!(text, "Jane");
}

/// The terminal view draws the caret after a pending insertion at its
/// offset, where typing goes; a deletion's text keeps the caret before it.
#[test]
fn the_terminal_caret_is_drawn_after_a_pending_insertion() {
    use docxcore::render::{RenderOptions, render_mapped};
    let mut ed = editor();
    insert_at(
        &mut ed,
        2,
        docxcore::field::fld_simple(" PAGE ", "777", &RunProps::default()),
    );
    let (lines, maps) = render_mapped(&ed.doc, &RenderOptions::default());
    assert_eq!(lines[0].plain(), "ab777cd");
    assert_eq!(maps[0].col_for_caret(&[0], 2, None), Some(5));
    assert_eq!(maps[0].col_for_caret(&[0], 1, None), Some(1));
    assert_eq!(maps[0].col_for_caret(&[0], 3, None), Some(6));

    let deleted = Editor::new(parse(
        "<w:p><w:r><w:t>ab</w:t></w:r><w:del w:id=\"1\" w:author=\"Bo\"><w:r>\
         <w:delText>777</w:delText></w:r></w:del><w:r><w:t>cd</w:t></w:r></w:p>",
    ));
    let (lines, maps) = render_mapped(&deleted.doc, &RenderOptions::default());
    assert_eq!(lines[0].plain(), "ab777cd");
    assert_eq!(maps[0].col_for_caret(&[0], 2, None), Some(2));
}
