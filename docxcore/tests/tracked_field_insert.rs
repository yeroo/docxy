//! #1625: a field inserted while tracking (Insert Field, Date & Time, a
//! paste) is recorded as a `w:ins` of its own, as a picture is (#1242):
//! zero-width until accepted. The caret is after it at its offset: typing
//! there goes after it, its own author's Backspace right after it removes it
//! outright and Delete acts on what follows, so a tracked delete never turns
//! it into a `w:del` that Reject brings back.

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
    let map = &maps[0];
    assert_eq!(map.col_for_caret(&[0], 2, None), Some(5));
    assert_eq!(map.col_for_caret(&[0], 1, None), Some(1));
    assert_eq!(map.col_for_caret(&[0], 3, None), Some(6));
    // A click on or inside the object's text lands at its offset (r4).
    for col in [2, 3, 4, 5] {
        assert_eq!(
            map.nearest_caret(col).map(|c| c.offset),
            Some(2),
            "col {col}"
        );
    }
    assert_eq!(map.nearest_caret(1).map(|c| c.offset), Some(1));
    // One arrow press steps over it, onto the caret after it.
    for (from, right) in [(1, true), (3, false)] {
        let to = map
            .visual_neighbor(&[0], from, None, right)
            .expect("a stop");
        assert_eq!((to.offset, to.col), (2, 5), "from {from}");
    }
    let back = map.visual_neighbor(&[0], 2, None, false).expect("a stop");
    assert_eq!(back.offset, 1);

    let deleted = Editor::new(parse(
        "<w:p><w:r><w:t>ab</w:t></w:r><w:del w:id=\"1\" w:author=\"Bo\"><w:r>\
         <w:delText>777</w:delText></w:r></w:del><w:r><w:t>cd</w:t></w:r></w:p>",
    ));
    let (lines, maps) = render_mapped(&deleted.doc, &RenderOptions::default());
    assert_eq!(lines[0].plain(), "ab777cd");
    assert_eq!(maps[0].col_for_caret(&[0], 2, None), Some(2));
}

/// The fields and pictures in `content`, wherever they sit (a link, a
/// tracked change).
fn objects_in(content: &[Inline]) -> usize {
    content
        .iter()
        .map(|i| match i {
            Inline::Field { .. } => 1,
            Inline::Raw(raw) if raw.contains("<w:drawing") => 1,
            Inline::Hyperlink(h) => objects_in(&h.content),
            Inline::Revision { content, .. } => objects_in(content),
            _ => 0,
        })
        .sum()
}

fn doc_objects(ed: &Editor) -> usize {
    ed.doc
        .body
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => objects_in(&p.content),
            _ => 0,
        })
        .sum()
}

/// The pending object insertions by `author` in the document.
fn pending_by(ed: &Editor, author: &str) -> usize {
    fn walk(content: &[Inline], author: &str) -> usize {
        content
            .iter()
            .map(|i| match i {
                Inline::Hyperlink(h) => walk(&h.content, author),
                Inline::Revision { metadata, .. }
                    if docxcore::editor::is_pending_object(i)
                        && metadata.author.as_deref() == Some(author) =>
                {
                    1
                }
                _ => 0,
            })
            .sum()
    }
    ed.doc
        .body
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => walk(&p.content, author),
            _ => 0,
        })
        .sum()
}

/// Paragraphs with pending insertions at their edges, in a link, in an
/// empty link, alone in a paragraph, and one by another author.
fn cross_check_doc() -> Document {
    let ins = |id: u32, author: &str, inner: &str| {
        format!("<w:ins w:id=\"{id}\" w:author=\"{author}\">{inner}</w:ins>")
    };
    parse(&format!(
        "<w:p>{}<w:r><w:t>ab</w:t></w:r>\
         <w:hyperlink w:anchor=\"x\"><w:r><w:t>L</w:t></w:r>{}</w:hyperlink>\
         <w:hyperlink w:anchor=\"y\">{}</w:hyperlink><w:r><w:t>cd</w:t></w:r>{}</w:p>\
         <w:p>{}<w:r><w:t>ef</w:t></w:r>{}<w:r><w:t>g</w:t></w:r></w:p>\
         <w:p>{}</w:p>\
         <w:p><w:r><w:t>h</w:t></w:r></w:p>",
        ins(1, "Ada", COMPLEX),
        ins(2, "Ada", COMPLEX),
        ins(3, "Ada", COMPLEX),
        ins(4, "Ada", COMPLEX),
        ins(5, "Ada", PIC),
        ins(6, "Bo", COMPLEX),
        ins(7, "Ada", COMPLEX),
    ))
}

/// Every caret position of [`cross_check_doc`], in document order.
fn positions() -> Vec<Caret> {
    let ed = Editor::new(cross_check_doc());
    let mut out = Vec::new();
    for (k, block) in ed.doc.body.iter().enumerate() {
        if let Block::Paragraph(p) = block {
            let len: usize = p.content.iter().map(docxcore::editor::inline_len).sum();
            out.extend((0..=len).map(|o| Caret::at(vec![k], o)));
        }
    }
    out
}

fn selected(lo: &Caret, hi: &Caret, tracking: bool) -> Editor {
    let mut ed = Editor::new(cross_check_doc());
    if tracking {
        track(&mut ed, "Ada");
    }
    ed.anchor = Some(lo.clone());
    ed.caret = hi.clone();
    ed
}

/// For every selection, a tracked cut removes exactly the pending objects
/// its clip carries (never loses one, never doubles one), and a tracked
/// delete removes the same ones.
#[test]
fn a_cut_removes_exactly_the_pending_objects_its_clip_carries() {
    let pos = positions();
    let mut checked = 0;
    for (i, lo) in pos.iter().enumerate() {
        for hi in &pos[i + 1..] {
            let mut ed = selected(lo, hi, true);
            let before = doc_objects(&ed);
            let (ada, bo) = (pending_by(&ed, "Ada"), pending_by(&ed, "Bo"));
            let carried = ed
                .cut()
                .map_or(0, |clip| clip.paras.iter().map(|p| objects_in(p)).sum());
            let at = format!("{lo:?}..{hi:?}");
            assert_eq!(doc_objects(&ed) + carried, before, "{at}");
            assert_eq!(ada - pending_by(&ed, "Ada"), carried, "{at}");
            assert_eq!(pending_by(&ed, "Bo"), bo, "{at}");
            let mut deleted = selected(lo, hi, true);
            deleted.delete_selection();
            assert_eq!(xml(&deleted), xml(&ed), "{at}");
            checked += 1;
        }
    }
    assert!(checked > 50, "{checked}");
}

#[test]
fn a_cell_range_cut_carries_a_pending_field_at_a_cells_start() {
    let mut ed = Editor::new(parse(
        "<w:tbl><w:tr><w:tc><w:p/></w:tc><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>\
         </w:tr></w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p>",
    ));
    track(&mut ed, "Ada");
    ed.set_caret(Caret::at(vec![0, 0, 0, 0], 0));
    ed.paste(&Clip::new(vec![vec![simple_field()]]));
    assert_eq!(insertions(&xml(&ed)).len(), 1, "{}", xml(&ed));
    ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
    ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
    let clip = ed.cut().expect("a clip");
    assert_eq!(
        clip.paras.iter().map(|p| objects_in(p)).sum::<usize>(),
        1,
        "{clip:?}"
    );
    assert!(!xml(&ed).contains("instrText"), "{}", xml(&ed));
    ed.anchor = None;
    ed.set_caret(Caret::at(vec![1], 5));
    ed.paste(&clip);
    assert_eq!(xml(&ed).matches("<w:instrText").count(), 1, "{}", xml(&ed));
}

#[test]
fn copy_carries_a_pending_field_or_picture() {
    for object in [simple_field(), picture()] {
        let mut ed = editor();
        insert_at(&mut ed, 2, object);
        let original = xml(&ed);
        ed.anchor = Some(Caret::at(vec![0], 1));
        ed.caret = Caret::at(vec![0], 3);
        let clip = ed.copy().expect("a clip");
        assert_eq!(objects_in(&clip.paras[0]), 1, "{clip:?}");
        assert_eq!(xml(&ed), original, "a copy changes nothing");
        // Pasted untracked, it is live.
        let mut other = Editor::new(parse("<w:p><w:r><w:t>z</w:t></w:r></w:p>"));
        other.set_caret(Caret::at(vec![0], 1));
        other.paste(&clip);
        assert_eq!(
            shape(&other)
                .iter()
                .filter(|s| s.starts_with("ins"))
                .count(),
            0
        );
        assert_eq!(objects_in(content(&other)), 1, "{}", xml(&other));
    }
    // Whoever's it is; one at the selection's start is not covered.
    let mut ed = editor();
    insert_at(&mut ed, 2, simple_field());
    track(&mut ed, "Bo");
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![0], 2);
    assert_eq!(objects_in(&ed.copy().unwrap().paras[0]), 1);
    ed.anchor = Some(Caret::at(vec![0], 2));
    ed.caret = Caret::at(vec![0], 3);
    assert_eq!(objects_in(&ed.copy().unwrap().paras[0]), 0);
}

/// A table of one row whose cells hold `cells` (each a cell's paragraphs'
/// XML), then a paragraph.
fn table(cells: &[&str]) -> Editor {
    let tcs: String = cells.iter().map(|c| format!("<w:tc>{c}</w:tc>")).collect();
    Editor::new(parse(&format!(
        "<w:tbl><w:tr>{tcs}</w:tr></w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p>"
    )))
}

fn para(text: &str) -> String {
    if text.is_empty() {
        "<w:p/>".into()
    } else {
        format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
    }
}

/// #1625 r3: a cell-range copy keeps one clip paragraph per cell paragraph,
/// empty ones included, as before #1625.
#[test]
fn a_cell_range_copy_keeps_empty_cell_paragraphs() {
    let ed = |cells: &[&str], last: usize| {
        let mut ed = table(cells);
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 0, last, 0], 0);
        ed
    };
    let (a, empty, c) = (para("A"), para(""), para("C"));
    let clip = ed(&[&a, &empty, &c], 2).copy().expect("a clip");
    assert_eq!(clip.to_text(), "A\n\nC");
    // A cell holding a blank paragraph between two.
    let two = format!("{}{}{}", para("a"), para(""), para("b"));
    let clip = ed(&[&two, &c], 1).copy().expect("a clip");
    assert_eq!(clip.paras.len(), 4);
    assert_eq!(clip.to_text(), "a\n\nb\nC");
    // Only empty cells: a clip of empty paragraphs.
    let clip = ed(&[&empty, &empty], 1).copy().expect("a clip");
    assert_eq!(clip.paras.len(), 2);
}

#[test]
fn a_cell_range_cut_over_empty_cells_still_clears_them() {
    let marked = "<w:p><w:bookmarkStart w:id=\"0\" w:name=\"b\"/><w:bookmarkEnd w:id=\"0\"/></w:p>";
    let mut ed = table(&[marked, &para(""), &para("x")]);
    ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
    ed.caret = Caret::at(vec![0, 0, 1, 0], 0);
    let clip = ed.cut().expect("a clip");
    assert_eq!(clip.paras.len(), 2);
    assert!(!xml(&ed).contains("w:bookmarkStart"), "{}", xml(&ed));
    assert!(ed.undo(), "one step");
    assert!(xml(&ed).contains("w:bookmarkStart"));
}

/// A plain copy over three paragraphs with an empty middle one gives the
/// clip it gave before #1625: the empty paragraph holds no span.
#[test]
fn a_copy_over_an_empty_middle_paragraph_is_unchanged() {
    let mut ed = Editor::new(parse(&format!("{}{}{}", para("ab"), para(""), para("cd"))));
    ed.anchor = Some(Caret::at(vec![0], 1));
    ed.caret = Caret::at(vec![2], 1);
    let clip = ed.copy().expect("a clip");
    assert_eq!(clip.to_text(), "b\nc");
    assert_eq!(clip.paras.len(), 2);
}

/// Every glyph reversed, at an odd (right-to-left) level: a whole line of
/// right-to-left text, as far as caret stops go.
#[derive(Debug)]
struct AllRtl;

impl docxcore::render::BidiProjector for AllRtl {
    fn project_line(
        &self,
        _: docxcore::render::BidiBaseDirection,
        glyphs: &[docxcore::render::BidiInputGlyph],
    ) -> docxcore::render::BidiVisualLine {
        let n = glyphs.len();
        docxcore::render::BidiVisualLine {
            clusters: (0..n)
                .rev()
                .enumerate()
                .map(|(k, i)| docxcore::render::BidiVisualCluster {
                    glyphs: vec![i],
                    logical: glyphs[i].logical_offset.map(|o| o..o + 1),
                    level: 1,
                    cells: k..k + 1,
                })
                .collect(),
            width: n,
        }
    }
}

/// On a right-to-left line the caret after a pending insertion is on its
/// left, so that is where it is drawn (r4 m3).
#[test]
fn the_terminal_caret_follows_a_pending_insertion_right_to_left() {
    use docxcore::render::{RenderOptions, render_mapped};
    let mut ed = editor();
    insert_at(
        &mut ed,
        2,
        docxcore::field::fld_simple(" PAGE ", "777", &RunProps::default()),
    );
    let opts = RenderOptions {
        bidi: Some(std::rc::Rc::new(AllRtl)),
        ..RenderOptions::default()
    };
    let (lines, maps) = render_mapped(&ed.doc, &opts);
    let line = lines[0].plain();
    let at = line.find("dc777ba").expect("reversed");
    let map = &maps[0];
    // `d c 7 7 7 b a`: offset 2 is between `c` and the field's text.
    assert_eq!(map.col_for_caret(&[0], 2, None), Some(at + 2), "{line:?}");
    for col in [at + 2, at + 3, at + 4, at + 5] {
        assert_eq!(
            map.nearest_caret(col).map(|c| c.offset),
            Some(2),
            "col {col}"
        );
    }
}

/// Typing right before a link that opens with a pending insertion goes after
/// the object, as everywhere else the caret is after one (r4): past a link
/// holding nothing else (outside it), into one with text (ahead of its text).
#[test]
fn text_typed_at_a_link_opening_with_a_pending_insertion_follows_it() {
    let ins = format!("<w:ins w:id=\"1\" w:author=\"Ada\">{COMPLEX}</w:ins>");
    for (link, inside) in [
        (ins.clone(), false),
        (format!("{ins}<w:r><w:t>L</w:t></w:r>"), true),
    ] {
        let mut ed = Editor::new(parse(&format!(
            "<w:p><w:r><w:t>ab</w:t></w:r><w:hyperlink w:anchor=\"x\">{link}</w:hyperlink>\
             <w:r><w:t>cd</w:t></w:r></w:p>"
        )));
        track(&mut ed, "Ada");
        ed.set_caret(Caret::at(vec![0], 2));
        ed.insert_char('x');
        assert_eq!(ed.caret.offset, 3);
        let x = xml(&ed);
        let typed = x.find(">x<").expect("the typed text");
        assert!(x.find("</w:ins>").unwrap() < typed, "after the field: {x}");
        let link_end = x.find("</w:hyperlink>").unwrap();
        assert_eq!(typed < link_end, inside, "{x}");
        if inside {
            assert!(
                typed < x.find(">L<").unwrap(),
                "ahead of the link's text: {x}"
            );
        }
        assert_eq!(
            text(&ed).replace('7', ""),
            if inside { "abxLcd" } else { "abxcd" }
        );
    }
}

/// `text`, then a pending field showing `777` at its end, tracking as Ada.
fn ab_then_field(text: &str) -> Editor {
    let mut ed = Editor::new(parse(&format!(
        "<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>"
    )));
    track(&mut ed, "Ada");
    let at = text.chars().count();
    insert_at(
        &mut ed,
        at,
        docxcore::field::fld_simple(" PAGE ", "777", &RunProps::default()),
    );
    ed
}

/// At a paragraph's end the caret after a pending insertion is drawn after
/// its text too, as typing goes there (r4 addendum).
#[test]
fn the_terminal_caret_after_a_pending_insertion_ending_a_paragraph() {
    use docxcore::render::{RenderOptions, render_mapped};
    let ed = ab_then_field("ab");
    let (lines, maps) = render_mapped(&ed.doc, &RenderOptions::default());
    assert_eq!(lines[0].plain().trim_end(), "ab777");
    let map = &maps[0];
    assert_eq!(map.col_for_caret(&[0], 2, None), Some(5));
    for col in [2, 3, 4, 5] {
        assert_eq!(
            map.nearest_caret(col).map(|c| c.offset),
            Some(2),
            "col {col}"
        );
    }
    let left = map.visual_neighbor(&[0], 2, None, false).expect("a stop");
    assert_eq!((left.offset, left.col), (1, 1));
    let right = map.visual_neighbor(&[0], 1, None, true).expect("a stop");
    assert_eq!((right.offset, right.col), (2, 5));

    // Right to left: after it is on its left.
    let opts = RenderOptions {
        bidi: Some(std::rc::Rc::new(AllRtl)),
        ..RenderOptions::default()
    };
    let (lines, maps) = render_mapped(&ed.doc, &opts);
    let line = lines[0].plain();
    let at = line.find("777ba").expect("reversed");
    assert_eq!(maps[0].col_for_caret(&[0], 2, None), Some(at), "{line:?}");
    for col in [at, at + 1, at + 2, at + 3] {
        assert_eq!(
            maps[0].nearest_caret(col).map(|c| c.offset),
            Some(2),
            "col {col}"
        );
    }
}

/// A pending insertion that ends a wrapped line (`ab 777 ` | `cdefgh`: a
/// line breaks at a space, so one follows it there): the caret at its
/// offset is drawn after its text on that line.
#[test]
fn the_terminal_caret_after_a_pending_insertion_ending_a_wrapped_line() {
    use docxcore::render::{RenderOptions, render_mapped};
    let mut ed = ab_then_field("ab ");
    ed.set_caret(Caret::at(vec![0], 3));
    ed.insert_str(" cdefgh");
    let opts = RenderOptions {
        width: 8,
        ..RenderOptions::default()
    };
    let (lines, maps) = render_mapped(&ed.doc, &opts);
    let shown: Vec<String> = lines.iter().map(|l| l.plain()).collect();
    let row = shown
        .iter()
        .position(|l| l.trim_end().ends_with("777"))
        .unwrap_or_else(|| panic!("a line ending with the field: {shown:?}"));
    let end = shown[row].trim_end().len();
    assert_eq!(
        maps[row].col_for_caret(&[0], 3, None),
        Some(end),
        "{shown:?}"
    );
    assert_eq!(
        maps[row].nearest_caret(end - 2).map(|c| c.offset),
        Some(3),
        "{shown:?}"
    );
}

/// A line showing only a pending field (after a line break) holds the
/// field's offset, not offset 0 (r5).
#[test]
fn a_line_showing_only_a_pending_insertion_holds_its_offset() {
    use docxcore::render::{RenderOptions, render_mapped};
    let mut ed = Editor::new(parse("<w:p><w:r><w:t>abc</w:t><w:br/></w:r></w:p>"));
    track(&mut ed, "Ada");
    insert_at(
        &mut ed,
        4,
        docxcore::field::fld_simple(" PAGE ", "777", &RunProps::default()),
    );
    let (lines, maps) = render_mapped(&ed.doc, &RenderOptions::default());
    let shown: Vec<String> = lines.iter().map(|l| l.plain()).collect();
    let row = shown
        .iter()
        .position(|l| l.trim_end() == "777")
        .unwrap_or_else(|| panic!("{shown:?}"));
    let abc = shown.iter().position(|l| l.starts_with("abc")).unwrap();
    assert_eq!(maps[row].col_for_caret(&[0], 4, None), Some(3), "{shown:?}");
    assert_eq!(maps[row].col_for_caret(&[0], 0, None), None, "{shown:?}");
    assert_eq!(maps[abc].col_for_caret(&[0], 0, None), Some(0), "{shown:?}");
    assert_eq!(maps[row].nearest_caret(1).map(|c| c.offset), Some(4));
}

/// Right-to-left text with its digits at level 2, as a real projector puts
/// them: the groups swap places, a letters group reads backwards and a
/// digits group forwards.
#[derive(Debug)]
struct RtlWithLtrDigits;

impl docxcore::render::BidiProjector for RtlWithLtrDigits {
    fn project_line(
        &self,
        _: docxcore::render::BidiBaseDirection,
        glyphs: &[docxcore::render::BidiInputGlyph],
    ) -> docxcore::render::BidiVisualLine {
        let digit = |i: usize| glyphs[i].ch.is_ascii_digit();
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for i in 0..glyphs.len() {
            match groups.last_mut() {
                Some(last) if digit(last[0]) == digit(i) => last.push(i),
                _ => groups.push(vec![i]),
            }
        }
        let mut order = Vec::new();
        for g in groups.into_iter().rev() {
            if digit(g[0]) {
                order.extend(g);
            } else {
                order.extend(g.into_iter().rev());
            }
        }
        docxcore::render::BidiVisualLine {
            clusters: order
                .iter()
                .enumerate()
                .map(|(k, &i)| docxcore::render::BidiVisualCluster {
                    glyphs: vec![i],
                    logical: glyphs[i].logical_offset.map(|o| o..o + 1),
                    level: if digit(i) { 2 } else { 1 },
                    cells: k..k + 1,
                })
                .collect(),
            width: order.len(),
        }
    }
}

/// A pending field showing digits in right-to-left text: the caret after it
/// is on its left, where the text around it goes on (r5 m2).
#[test]
fn the_caret_after_a_pending_field_follows_the_text_around_it() {
    use docxcore::render::{RenderOptions, render_mapped};
    for (text, at) in [("אבגד", 2), ("אב", 2)] {
        let mut ed = Editor::new(parse(&format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")));
        track(&mut ed, "Ada");
        insert_at(
            &mut ed,
            at,
            docxcore::field::fld_simple(" PAGE ", "777", &RunProps::default()),
        );
        let opts = RenderOptions {
            bidi: Some(std::rc::Rc::new(RtlWithLtrDigits)),
            ..RenderOptions::default()
        };
        let (lines, maps) = render_mapped(&ed.doc, &opts);
        let line = lines[0].plain();
        // `ד ג 7 7 7 ב א` (or `7 7 7 ב א`): after the field is left of it.
        let digits = line.find('7').expect("the field");
        let col = line[..digits].chars().count();
        assert_eq!(maps[0].col_for_caret(&[0], at, None), Some(col), "{line:?}");
    }
}
