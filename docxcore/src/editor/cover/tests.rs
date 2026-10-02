use super::*;
use crate::cover::{Placeholder, is_cover_open};
use crate::load::{Relationships, parse_document_xml};
use crate::serialize::blocks_to_xml;

const SECT: &str = "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>";

fn ed(inner: &str) -> Editor {
    let xml = format!(
        "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{inner}</w:body></w:document>"
    );
    Editor::new(parse_document_xml(&xml, &Relationships::default()))
}

fn text_at(e: &Editor, path: &[usize]) -> String {
    resolve_para(&e.doc.body, path)
        .map(|p| p.plain_text())
        .unwrap_or_default()
}

fn covers(e: &Editor) -> usize {
    e.doc.body.iter().filter(|b| is_cover_open(b)).count()
}

/// The top-level index of the cover paragraph holding `field`.
fn field_para(e: &Editor, field: Placeholder) -> usize {
    e.doc
        .body
        .iter()
        .position(|b| {
            matches!(b, Block::Paragraph(p) if p.content.iter().any(
                |i| matches!(i, Inline::Raw(r) if r.contains(&format!("w:alias w:val=\"{}\"", field.alias())))
            ))
        })
        .unwrap_or_else(|| panic!("no {field:?} placeholder"))
}

/// Select the whole text of the placeholder paragraph at `block` and type
/// `text` over it, as a person replaces a prompt.
fn type_over(e: &mut Editor, block: usize, text: &str) {
    let len = text_at(e, &[block]).chars().count();
    e.anchor = Some(Caret {
        path: vec![block],
        offset: 0,
    });
    e.caret = Caret {
        path: vec![block],
        offset: len,
    };
    e.insert_str(text);
}

fn body_doc() -> Editor {
    ed(&format!(
        "<w:p><w:r><w:t>Body text</w:t></w:r></w:p><w:p><w:r><w:t>More</w:t></w:r></w:p>{SECT}"
    ))
}

#[test]
fn insert_puts_a_cover_first_sets_title_page_and_undoes_in_one_step() {
    let mut e = body_doc();
    e.caret = Caret {
        path: vec![1],
        offset: 2,
    };
    let before = e.doc.clone();
    e.set_cover_page(0, &[]).unwrap();
    assert!(is_cover_open(&e.doc.body[0]));
    assert!(e.has_cover_page());
    assert_eq!(covers(&e), 1);
    assert!(crate::sect::has_flag(
        e.sections().last().unwrap(),
        "w:titlePg"
    ));
    // The caret still sits in "More", two characters in.
    assert_eq!(text_at(&e, &e.caret.path), "More");
    assert_eq!(e.caret.offset, 2);
    let xml = blocks_to_xml(&e.doc.body);
    assert!(xml.contains("<w:docPartGallery w:val=\"Cover Pages\"/><w:docPartUnique/>"));
    assert!(xml.contains("[Document title]"), "{xml}");

    assert!(e.undo());
    assert_eq!(e.doc, before);
    assert_eq!(text_at(&e, &e.caret.path), "More");
    assert!(!e.undo(), "one step");
}

#[test]
fn title_page_goes_on_the_first_section_of_several() {
    let first = "<w:sectPr><w:type w:val=\"continuous\"/></w:sectPr>";
    let mut e = ed(&format!(
        "<w:p><w:pPr>{first}</w:pPr><w:r><w:t>one</w:t></w:r></w:p><w:p><w:r><w:t>two</w:t></w:r></w:p>{SECT}"
    ));
    e.set_cover_page(1, &[]).unwrap();
    let sections = e.sections();
    assert!(
        crate::sect::has_flag(&sections[0], "w:titlePg"),
        "{sections:?}"
    );
    assert!(!crate::sect::has_flag(&sections[1], "w:titlePg"));
}

#[test]
fn typing_over_a_placeholder_stays_inside_it_and_is_carried_to_another_design() {
    let mut e = body_doc();
    e.set_cover_page(2, &[]).unwrap(); // Title Band: has Abstract and Company
    let title = field_para(&e, Placeholder::Title);
    type_over(&mut e, title, "Report");
    let abstract_ = field_para(&e, Placeholder::Abstract);
    type_over(&mut e, abstract_, "Summary");
    let xml = blocks_to_xml(&e.doc.body);
    assert!(
        xml.contains("<w:alias w:val=\"Title\"/><w:tag w:val=\"Title\"/>"),
        "{xml}"
    );
    // The typed text is inside the control, with the placeholder's formatting.
    let Block::Paragraph(p) = &e.doc.body[title] else {
        panic!()
    };
    let inside = p
        .content
        .iter()
        .skip_while(|i| !matches!(i, Inline::Raw(r) if crate::hf::is_sdt_open(r)))
        .find_map(|i| match i {
            Inline::Run(r) => Some(r),
            _ => None,
        })
        .expect("a run inside the control");
    assert_eq!(inside.text, "Report");
    assert!(inside.props.bold && inside.props.size_half_pts == Some(64));

    e.set_cover_page(3, &[]).unwrap(); // Ruled: no Abstract, no Company
    assert_eq!(covers(&e), 1);
    assert_eq!(text_at(&e, &[field_para(&e, Placeholder::Title)]), "Report");
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Subtitle)]),
        "[Document subtitle]"
    );
    let all: String = e.doc.body.iter().map(Block::plain_text).collect();
    assert!(!all.contains("Summary"), "{all}");
    // The body follows the cover, once.
    assert_eq!(all.matches("Body text").count(), 1);
    // Back to a design with an Abstract: the dropped text does not return.
    e.set_cover_page(2, &[]).unwrap();
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Abstract)]),
        "[Abstract]"
    );
    assert_eq!(text_at(&e, &[field_para(&e, Placeholder::Title)]), "Report");
}

const LABELLED: &str = "<w:p><w:r><w:t xml:space=\"preserve\">Title: </w:t></w:r>\
    <w:sdt><w:sdtPr><w:alias w:val=\"Title\"/></w:sdtPr><w:sdtContent><w:proofErr w:type=\"spellStart\"/>\
    <w:r><w:t>[x]</w:t></w:r></w:sdtContent></w:sdt></w:p>";

#[test]
fn typing_into_an_emptied_control_after_text_goes_inside() {
    let mut e = ed(LABELLED);
    e.anchor = Some(Caret {
        path: vec![0],
        offset: 7,
    });
    e.caret = Caret {
        path: vec![0],
        offset: 10,
    };
    e.insert_str("Mine");
    let xml = blocks_to_xml(&e.doc.body);
    assert!(
        xml.contains("<w:sdtContent><w:proofErr w:type=\"spellStart\"/><w:r><w:t xml:space=\"preserve\">Mine</w:t></w:r></w:sdtContent>")
            || xml.contains("<w:sdtContent><w:r><w:t xml:space=\"preserve\">Mine</w:t></w:r><w:proofErr"),
        "{xml}"
    );
    assert_eq!(e.caret.offset, 11);
}

#[test]
fn pasting_into_an_emptied_control_goes_inside() {
    let mut e = ed(LABELLED);
    e.anchor = Some(Caret {
        path: vec![0],
        offset: 7,
    });
    e.caret = Caret {
        path: vec![0],
        offset: 10,
    };
    e.paste(&Clip::from_text("Pasted"));
    let xml = blocks_to_xml(&e.doc.body);
    let inside = &xml[xml.find("<w:sdtContent>").unwrap()..xml.find("</w:sdtContent>").unwrap()];
    assert!(inside.contains("Pasted"), "{xml}");
}

#[test]
fn typing_at_the_edge_of_a_control_with_text_keeps_the_usual_rule() {
    let mut e = ed(LABELLED);
    e.caret = Caret {
        path: vec![0],
        offset: 7,
    };
    e.insert_str("!");
    let xml = blocks_to_xml(&e.doc.body);
    // Before the control, as before #652: only an empty control draws text in.
    assert!(xml.contains("Title: !</w:t></w:r><w:sdt>"), "{xml}");
}

#[test]
fn replacing_a_word_cover_carries_its_typed_text() {
    let mut e = ed(&format!(
        "{}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    e.set_cover_page(2, &[]).unwrap();
    assert_eq!(covers(&e), 1);
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Title)]),
        "Annual Report"
    );
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Subtitle)]),
        "[Document subtitle]"
    );
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Abstract)]),
        "First line.\nSecond line."
    );
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Date)]),
        "2026-10-02"
    );
    let xml = blocks_to_xml(&e.doc.body);
    assert!(
        !xml.contains("Logo") && !xml.contains("-1876379021"),
        "{xml}"
    );
}

#[test]
fn remove_takes_exactly_the_cover_in_one_step() {
    let pn = crate::hf::page_number_sdt_xml(&crate::hf::PAGE_NUMBER_DESIGNS[0], false, 9);
    let mut e = ed(&format!(
        "{}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{pn}{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    let last = e.doc.body.len() - 3; // the page number's paragraph
    e.caret = Caret {
        path: vec![last],
        offset: 0,
    };
    let before = e.doc.clone();
    assert!(e.remove_cover_page());
    assert!(!e.has_cover_page());
    let xml = blocks_to_xml(&e.doc.body);
    assert!(
        !xml.contains("Logo") && !xml.contains("w:type=\"page\""),
        "{xml}"
    );
    assert!(xml.contains("Page Numbers (Bottom of Page)") && xml.contains("Body text"));
    assert!(matches!(&e.doc.body[0], Block::Paragraph(p) if p.plain_text() == "Body text"));
    // The caret stayed on the page number's paragraph.
    assert_eq!(e.caret.path, vec![2]);
    assert!(e.undo());
    assert_eq!(e.doc, before);
    assert!(!e.undo(), "one step");

    // No cover: nothing happens and nothing is recorded.
    let mut plain = body_doc();
    assert!(!plain.remove_cover_page());
    assert!(!plain.undo());
}

#[test]
fn a_caret_inside_the_cover_goes_to_the_text_after_it() {
    let mut e = body_doc();
    e.set_cover_page(0, &[]).unwrap();
    let title = field_para(&e, Placeholder::Title);
    e.anchor = Some(Caret {
        path: vec![title],
        offset: 0,
    });
    e.caret = Caret {
        path: vec![title],
        offset: 3,
    };
    e.set_cover_page(1, &[]).unwrap();
    assert_eq!(text_at(&e, &e.caret.path), "Body text");
    assert_eq!(e.caret.offset, 0);
    assert_eq!(e.anchor, None);

    let title = field_para(&e, Placeholder::Title);
    e.caret = Caret {
        path: vec![title],
        offset: 2,
    };
    assert!(e.remove_cover_page());
    assert_eq!(e.caret.path, vec![0]);
    assert_eq!(text_at(&e, &e.caret.path), "Body text");
}

#[test]
fn a_selection_in_the_body_follows_it() {
    let mut e = body_doc();
    e.anchor = Some(Caret {
        path: vec![0],
        offset: 1,
    });
    e.caret = Caret {
        path: vec![1],
        offset: 2,
    };
    e.set_cover_page(0, &[]).unwrap();
    assert_eq!(e.selection_text(), "ody text\nMo");
    assert!(e.remove_cover_page());
    assert_eq!(e.selection_text(), "ody text\nMo");
}

#[test]
fn removing_a_cover_that_was_all_the_body_leaves_a_paragraph() {
    let mut e = ed(&format!("{}{SECT}", crate::cover::tests::WORD_COVER));
    assert!(e.remove_cover_page());
    assert!(matches!(e.doc.body[0], Block::Paragraph(_)));
    assert_eq!(e.caret.path, vec![0]);
}

#[test]
fn a_cover_survives_save_and_reload() {
    let mut e = body_doc();
    e.set_cover_page(0, &[41]).unwrap();
    let title = field_para(&e, Placeholder::Title);
    type_over(&mut e, title, "Saved title");
    let pkg = crate::package::new_package(e.doc.clone());
    let bytes = crate::package::save_package(&pkg);
    let back = crate::package::load_package(&bytes).expect("reload");
    let mut r = Editor::new(back.document);
    assert!(r.has_cover_page());
    let xml = blocks_to_xml(&r.doc.body);
    let mut ids = crate::cover::sdt_ids(&xml);
    assert!(ids.iter().all(|&id| id > 41), "{ids:?}");
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "unique ids");
    r.set_cover_page(3, &[]).unwrap();
    assert_eq!(
        text_at(&r, &[field_para(&r, Placeholder::Title)]),
        "Saved title"
    );
    assert!(r.remove_cover_page());
    assert!(!r.has_cover_page());
}

#[test]
fn blank_page_is_two_page_breaks_in_one_step() {
    let mut e = body_doc();
    e.caret = Caret {
        path: vec![0],
        offset: 4,
    };
    e.insert_blank_page();
    let Block::Paragraph(p) = &e.doc.body[0] else {
        panic!()
    };
    let breaks = p
        .content
        .iter()
        .filter(|i| matches!(i, Inline::Break(BreakKind::Page, _)))
        .count();
    assert_eq!(breaks, 2);
    assert_eq!(p.plain_text(), "Body\n\n text");
    assert_eq!(e.caret.offset, 6);
    assert!(e.undo());
    assert_eq!(text_at(&e, &[0]), "Body text");
    assert!(!e.undo(), "one step");
}
