//! #212: the plain text inside a "complex" hyperlink (one holding proofing
//! marks, bookmarks, tracked changes, …) is editable, searchable and saved
//! with every other child of the link kept in place.

use docxcore::agent;
use docxcore::editor::{Caret, Editor, Match, inline_len};
use docxcore::load::{Relationships, parse_document_xml, parse_rels_xml};
use docxcore::model::{Block, Document, Inline};
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// The issue's example: Word's spell-check marks around a link's text.
const PROOF_LINK: &str = "<w:hyperlink r:id=\"rId5\" w:history=\"1\">\
    <w:proofErr w:type=\"spellStart\"/><w:r><w:t>Contoso</w:t></w:r>\
    <w:proofErr w:type=\"spellEnd\"/></w:hyperlink>";

fn rels() -> Relationships {
    parse_rels_xml(
        "<Relationships><Relationship Id=\"rId5\" Target=\"https://contoso.test/\" \
         TargetMode=\"External\"/></Relationships>",
    )
}

fn load(p_inner: &str) -> Document {
    let xml = format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body><w:p>{p_inner}</w:p>\
         </w:body></w:document>"
    );
    parse_document_xml(&xml, &rels())
}

fn reload(doc: &Document) -> Document {
    parse_document_xml(&document_to_xml(doc), &rels())
}

/// `Visit ` + `link` + ` now.`
fn around(link: &str) -> String {
    format!(
        "<w:r><w:t xml:space=\"preserve\">Visit </w:t></w:r>{link}\
         <w:r><w:t xml:space=\"preserve\"> now.</w:t></w:r>"
    )
}

/// The first paragraph's text in editor offsets.
fn etext(doc: &Document) -> String {
    let mut ed = Editor::new(doc.clone());
    ed.select_all();
    ed.selection_text()
}

fn content(doc: &Document) -> &[Inline] {
    match &doc.body[0] {
        Block::Paragraph(p) => &p.content,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

fn link_text(doc: &Document) -> String {
    content(doc)
        .iter()
        .find_map(|i| match i {
            Inline::Hyperlink(_) => Some(i.text()),
            _ => None,
        })
        .expect("a hyperlink")
}

/// Each needle occurs in `hay`, in this order.
fn assert_in_order(hay: &str, needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        match hay[from..].find(needle) {
            Some(at) => from += at + needle.len(),
            None => panic!("{needle:?} missing or out of order in {hay}"),
        }
    }
}

fn at(offset: usize) -> Caret {
    Caret::at(vec![0], offset)
}

#[test]
fn text_inside_a_link_with_proofing_marks_is_found_and_replaced() {
    let doc = load(&around(PROOF_LINK));
    assert_eq!(inline_len(&content(&doc)[1]), 7);
    assert_eq!(etext(&doc), "Visit Contoso now.");
    assert_eq!(
        agent::find(&doc, "Contoso", false),
        vec![Match {
            path: vec![0],
            start: 6,
            end: 13
        }]
    );

    let mut ed = Editor::new(doc);
    assert_eq!(
        agent::replace_all(&mut ed, "Contoso", "Fabrikam", false),
        (1, 1)
    );
    let saved = document_to_xml(&ed.doc);
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\" w:history=\"1\">",
            "<w:proofErr w:type=\"spellStart\"/>",
            "Fabrikam",
            "<w:proofErr w:type=\"spellEnd\"/>",
            "</w:hyperlink>",
            " now.",
        ],
    );
    assert!(!saved.contains("Contoso"), "{saved}");

    let back = reload(&ed.doc);
    assert_eq!(etext(&back), "Visit Fabrikam now.");
    assert_eq!(link_text(&back), "Fabrikam");
}

#[test]
fn typing_and_deleting_inside_the_link_saves_the_new_text() {
    let mut ed = Editor::new(load(&around(PROOF_LINK)));
    ed.set_caret(at(9)); // Con|toso
    ed.insert_char('x');
    assert_eq!(etext(&ed.doc), "Visit Conxtoso now.");
    ed.set_caret(at(6));
    ed.delete_forward(); // C
    ed.set_caret(at(13));
    ed.backspace(); // the last o
    assert_eq!(etext(&ed.doc), "Visit onxtos now.");

    let saved = document_to_xml(&ed.doc);
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\" w:history=\"1\">",
            "<w:proofErr w:type=\"spellStart\"/>",
            "onxtos",
            "<w:proofErr w:type=\"spellEnd\"/>",
            "</w:hyperlink>",
        ],
    );
    assert_eq!(link_text(&reload(&ed.doc)), "onxtos");
}

#[test]
fn links_holding_bookmarks_external_or_anchored_are_editable() {
    for link in [
        "<w:hyperlink r:id=\"rId5\"><w:bookmarkStart w:id=\"3\" w:name=\"here\"/>\
         <w:r><w:t>Section</w:t></w:r><w:bookmarkEnd w:id=\"3\"/></w:hyperlink>",
        "<w:hyperlink w:anchor=\"sec\"><w:bookmarkStart w:id=\"3\" w:name=\"here\"/>\
         <w:r><w:t>Section</w:t></w:r><w:bookmarkEnd w:id=\"3\"/></w:hyperlink>",
    ] {
        let doc = load(&around(link));
        assert!(
            matches!(&content(&doc)[1], Inline::Hyperlink(h) if h.runs.is_empty()),
            "the loader keeps this link complex: {link}"
        );
        assert_eq!(
            agent::find(&doc, "Section", false),
            vec![Match {
                path: vec![0],
                start: 6,
                end: 13
            }],
            "{link}"
        );
        let mut ed = Editor::new(doc);
        assert_eq!(
            agent::replace_all(&mut ed, "Section", "Part", false),
            (1, 1)
        );
        let saved = document_to_xml(&ed.doc);
        let opener = &link[..link.find('>').unwrap() + 1];
        assert_in_order(
            &saved,
            &[
                opener,
                "<w:bookmarkStart w:id=\"3\" w:name=\"here\"/>",
                "Part",
                "<w:bookmarkEnd w:id=\"3\"/>",
                "</w:hyperlink>",
            ],
        );
        assert_eq!(etext(&reload(&ed.doc)), "Visit Part now.", "{link}");
    }
}

#[test]
fn an_untouched_complex_link_saves_byte_identical() {
    let doc = load(&around(PROOF_LINK));
    assert!(document_to_xml(&doc).contains(PROOF_LINK));

    // Formatting the text next to the link, but not the link, leaves it be.
    let mut ed = Editor::new(doc);
    ed.anchor = Some(at(0));
    ed.caret = at(6);
    ed.toggle_bold();
    let saved = document_to_xml(&ed.doc);
    assert!(saved.contains("<w:b/>"), "{saved}");
    assert!(saved.contains(PROOF_LINK), "{saved}");

    // Formatting over it rebuilds it from its (bold) content.
    ed.anchor = Some(at(5));
    ed.caret = at(8);
    ed.toggle_bold();
    let saved = document_to_xml(&ed.doc);
    assert!(!saved.contains(PROOF_LINK), "{saved}");
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\" w:history=\"1\">",
            "<w:proofErr w:type=\"spellStart\"/>",
            "<w:b/>",
            "Co",
            "ntoso",
            "</w:hyperlink>",
        ],
    );
}

#[test]
fn undoing_an_edit_inside_the_link_saves_byte_identical() {
    let mut ed = Editor::new(load(&around(PROOF_LINK)));
    let before = document_to_xml(&ed.doc);
    ed.set_caret(at(9));
    ed.insert_char('x');
    assert_ne!(document_to_xml(&ed.doc), before);
    assert!(ed.undo());
    assert_eq!(document_to_xml(&ed.doc), before);
}

/// A link holding a tracked change and plain text (the #197 fixture shape).
const REVIEW_LINK: &str = "<w:r><w:t xml:space=\"preserve\">See </w:t></w:r>\
    <w:hyperlink r:id=\"rId5\"><w:r><w:t>here</w:t></w:r>\
    <w:ins w:id=\"3\" w:author=\"A\"><w:r><w:t>more</w:t></w:r></w:ins></w:hyperlink>\
    <w:r><w:t xml:space=\"preserve\"> end.</w:t></w:r>";

fn edited_review_link() -> Editor {
    let mut ed = Editor::new(load(REVIEW_LINK));
    assert_eq!(etext(&ed.doc), "See here end.");
    assert_eq!(agent::replace_all(&mut ed, "here", "there", false), (1, 1));
    assert_eq!(etext(&ed.doc), "See there end.");
    ed.set_caret(at(0));
    let location = ed.next_revision().expect("the insertion");
    assert_eq!(
        location.start,
        at(9),
        "the insertion sits after `there`, not at the link start"
    );
    ed
}

#[test]
fn accepting_a_revision_inside_an_edited_link_keeps_both() {
    let mut ed = edited_review_link();
    assert!(ed.accept_current_revision().unwrap().is_applied());
    let saved = document_to_xml(&ed.doc);
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\">",
            "there",
            "more",
            "</w:hyperlink>",
        ],
    );
    assert!(!saved.contains("<w:ins"), "{saved}");
    assert_eq!(etext(&reload(&ed.doc)), "See theremore end.");
}

#[test]
fn rejecting_a_revision_inside_an_edited_link_keeps_the_edit() {
    let mut ed = edited_review_link();
    assert!(ed.reject_current_revision().unwrap().is_applied());
    let saved = document_to_xml(&ed.doc);
    assert_in_order(
        &saved,
        &["<w:hyperlink r:id=\"rId5\">", "there", "</w:hyperlink>"],
    );
    assert!(!saved.contains("more"), "{saved}");
    assert_eq!(etext(&reload(&ed.doc)), "See there end.");
}

#[test]
fn deleting_all_of_a_links_text_keeps_its_marks_in_place() {
    let mut ed = Editor::new(load(&around(PROOF_LINK)));
    ed.anchor = Some(at(6));
    ed.caret = at(13);
    assert!(ed.delete_selection());
    assert_eq!(etext(&ed.doc), "Visit  now.");
    let saved = document_to_xml(&ed.doc);
    assert!(!saved.contains("w:hyperlink"), "{saved}");
    assert_in_order(
        &saved,
        &[
            "Visit ",
            "<w:proofErr w:type=\"spellStart\"/>",
            "<w:proofErr w:type=\"spellEnd\"/>",
            " now.",
        ],
    );
}

#[test]
fn deleting_all_of_a_links_text_keeps_a_link_that_still_shows_a_change() {
    let mut ed = Editor::new(load(REVIEW_LINK));
    ed.anchor = Some(at(4));
    ed.caret = at(8);
    assert!(ed.delete_selection());
    assert_eq!(etext(&ed.doc), "See  end.");
    let saved = document_to_xml(&ed.doc);
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\">",
            "<w:ins w:id=\"3\" w:author=\"A\">",
            "more",
            "</w:ins></w:hyperlink>",
        ],
    );
    assert!(!saved.contains("here"), "{saved}");
    assert_eq!(reload(&ed.doc).plain_text(), "See more end.\n");
}

#[test]
fn editing_a_nested_link_rebuilds_both_links() {
    let doc = load(
        "<w:hyperlink r:id=\"rId5\" w:history=\"1\"><w:proofErr w:type=\"spellStart\"/>\
         <w:hyperlink w:anchor=\"x\"><w:bookmarkStart w:id=\"4\" w:name=\"b\"/>\
         <w:r><w:t>inner</w:t></w:r><w:bookmarkEnd w:id=\"4\"/></w:hyperlink>\
         </w:hyperlink>",
    );
    assert_eq!(etext(&doc), "inner");
    let mut ed = Editor::new(doc);
    assert_eq!(agent::replace_all(&mut ed, "inner", "outer", false), (1, 1));
    let saved = document_to_xml(&ed.doc);
    assert!(
        !saved.contains("inner"),
        "the stale raw of either link: {saved}"
    );
    assert_in_order(
        &saved,
        &[
            "<w:hyperlink r:id=\"rId5\" w:history=\"1\">",
            "<w:proofErr w:type=\"spellStart\"/>",
            "<w:hyperlink w:anchor=\"x\">",
            "<w:bookmarkStart w:id=\"4\" w:name=\"b\"/>",
            "outer",
            "<w:bookmarkEnd w:id=\"4\"/>",
            "</w:hyperlink></w:hyperlink>",
        ],
    );
    assert_eq!(etext(&reload(&ed.doc)), "outer");
}
