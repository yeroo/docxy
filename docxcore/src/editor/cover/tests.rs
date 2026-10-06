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
    let page_breaks = |b: &Block| match b {
        Block::Paragraph(p) => p
            .content
            .iter()
            .filter(|i| matches!(i, Inline::Break(BreakKind::Page, _)))
            .count(),
        _ => 0,
    };
    // "Body" ends its page, the blank page is a paragraph of its own, and
    // " text" starts the page after it, with the caret.
    let breaks: Vec<usize> = e.doc.body[..3].iter().map(page_breaks).collect();
    assert_eq!(breaks, [1, 1, 0]);
    assert_eq!(
        text_at(&e, &[0]),
        "Body
"
    );
    assert_eq!(
        text_at(&e, &[1]),
        "
"
    );
    assert_eq!(text_at(&e, &[2]), " text");
    assert_eq!(e.caret.path, vec![2]);
    assert_eq!(e.caret.offset, 0);
    assert!(e.undo());
    assert_eq!(text_at(&e, &[0]), "Body text");
    assert_eq!(e.doc.body.len(), 3, "two paragraphs and the sectPr");
    assert!(!e.undo(), "one step");
}

/// Whether every paragraph's inline content controls open and close in pairs,
/// so the body serializes to well-formed XML.
fn inline_controls_balance(e: &Editor) -> bool {
    e.doc.body.iter().all(|b| {
        let Block::Paragraph(p) = b else {
            return true;
        };
        let mut depth = 0i32;
        for i in &p.content {
            match i {
                Inline::Raw(r) if crate::hf::is_sdt_open(r) => depth += 1,
                Inline::Raw(r) if crate::hf::is_sdt_close(r) => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        depth == 0
    })
}

/// Save and reload through the package, as a person would.
fn reloaded(e: &Editor) -> Editor {
    let pkg = crate::package::new_package(e.doc.clone());
    let bytes = crate::package::save_package(&pkg);
    Editor::new(
        crate::package::load_package(&bytes)
            .expect("reload")
            .document,
    )
}

/// A Plain cover with "My title" typed into its Title, the caret in the
/// title paragraph at `offset`.
fn typed_title(offset: usize) -> (Editor, usize) {
    let mut e = body_doc();
    e.set_cover_page(0, &[]).unwrap();
    let title = field_para(&e, Placeholder::Title);
    type_over(&mut e, title, "My title");
    e.anchor = None;
    e.caret = Caret {
        path: vec![title],
        offset,
    };
    (e, title)
}

fn all_text(e: &Editor) -> String {
    e.doc.body.iter().map(Block::plain_text).collect()
}

#[test]
fn enter_inside_a_placeholder_closes_and_reopens_it() {
    let (mut e, title) = typed_title(2);
    e.insert_newline();
    assert!(
        inline_controls_balance(&e),
        "{}",
        blocks_to_xml(&e.doc.body)
    );
    assert_eq!(text_at(&e, &[title]), "My");
    assert_eq!(text_at(&e, &[title + 1]), " title");
    // The reopened half has no id of its own, so ids stay unique.
    let xml = blocks_to_xml(&e.doc.body);
    let mut ids = crate::cover::sdt_ids(&xml);
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "{xml}");
    assert_eq!(xml.matches("<w:alias w:val=\"Title\"/>").count(), 2);

    let mut r = reloaded(&e);
    assert!(inline_controls_balance(&r));
    assert_eq!(all_text(&r), all_text(&e));
    // Both pieces carry into another design, a line apart.
    r.set_cover_page(1, &[]).unwrap();
    assert_eq!(
        text_at(&r, &[field_para(&r, Placeholder::Title)]),
        "My\ntitle"
    );
}

#[test]
fn blank_page_inside_a_placeholder_keeps_it_balanced() {
    for offset in [2, 0] {
        let (mut e, _) = typed_title(offset);
        e.insert_blank_page();
        assert!(
            inline_controls_balance(&e),
            "offset {offset}: {}",
            blocks_to_xml(&e.doc.body)
        );
        let r = reloaded(&e);
        assert!(inline_controls_balance(&r));
        assert_eq!(all_text(&r), all_text(&e), "offset {offset}");
        assert!(r.has_cover_page());
    }
}

#[test]
fn a_paste_of_paragraphs_inside_a_placeholder_keeps_it_balanced() {
    let (mut e, title) = typed_title(2);
    e.paste(&Clip::from_text("one\ntwo\nthree"));
    assert!(
        inline_controls_balance(&e),
        "{}",
        blocks_to_xml(&e.doc.body)
    );
    assert_eq!(text_at(&e, &[title]), "Myone");
    assert_eq!(text_at(&e, &[title + 2]), "three title");
    let r = reloaded(&e);
    assert!(inline_controls_balance(&r));
    assert_eq!(all_text(&r), all_text(&e));
}

#[test]
fn the_caret_at_an_emptied_placeholder_reads_its_formatting() {
    let mut e = body_doc();
    e.set_cover_page(2, &[]).unwrap(); // Title Band: bold 32 pt title
    let title = field_para(&e, Placeholder::Title);
    let len = text_at(&e, &[title]).chars().count();
    e.anchor = Some(Caret {
        path: vec![title],
        offset: 0,
    });
    e.caret = Caret {
        path: vec![title],
        offset: len,
    };
    e.delete_selection();
    let props = e.caret_props();
    assert!(props.bold && props.size_half_pts == Some(64), "{props:?}");
    // A tab there goes inside the control, formatted as typing would be.
    e.insert_tab();
    let Block::Paragraph(p) = &e.doc.body[title] else {
        panic!()
    };
    let at = |pred: &dyn Fn(&Inline) -> bool| p.content.iter().position(pred).unwrap();
    let open = at(&|i| matches!(i, Inline::Raw(r) if crate::hf::is_sdt_open(r)));
    let close = at(&|i| matches!(i, Inline::Raw(r) if crate::hf::is_sdt_close(r)));
    let tab = at(&|i| matches!(i, Inline::Tab(_)));
    assert!(open < tab && tab < close, "{:?}", p.content);
    let Inline::Tab(props) = &p.content[tab] else {
        unreachable!()
    };
    assert!(props.bold && props.size_half_pts == Some(64));
}

#[test]
fn title_page_goes_on_the_section_the_cover_is_in() {
    let first = "<w:sectPr><w:type w:val=\"continuous\"/></w:sectPr>";
    let mut e = ed(&format!(
        "<w:p><w:pPr>{first}</w:pPr><w:r><w:t>Before</w:t></w:r></w:p>{}\
         <w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    let before = e.sections()[0].clone();
    e.set_cover_page(1, &[]).unwrap();
    let sections = e.sections();
    assert_eq!(sections[0], before, "section 0 is not the cover's");
    assert!(
        crate::sect::has_flag(&sections[1], "w:titlePg"),
        "{sections:?}"
    );
}

/// "Title:" in italics, then an emptied bold Title control.
const BEFORE_EMPTY: &str = "<w:p><w:r><w:rPr><w:i/></w:rPr><w:t>Title:</w:t></w:r>\
    <w:sdt><w:sdtPr><w:rPr><w:b/></w:rPr><w:alias w:val=\"Title\"/></w:sdtPr>\
    <w:sdtContent></w:sdtContent></w:sdt></w:p>";

#[test]
fn replacing_text_next_to_an_emptied_control_leaves_it_empty() {
    let check = |e: &Editor| {
        let xml = blocks_to_xml(&e.doc.body);
        assert!(
            xml.contains("<w:sdtContent></w:sdtContent>"),
            "the control stays empty: {xml}"
        );
        let Block::Paragraph(p) = &e.doc.body[0] else {
            panic!()
        };
        let Inline::Run(r) = &p.content[0] else {
            panic!("{xml}")
        };
        assert_eq!(r.text, "Title;");
        assert!(r.props.italic && !r.props.bold, "{xml}");
    };
    let mut e = ed(BEFORE_EMPTY);
    e.anchor = Some(Caret {
        path: vec![0],
        offset: 5,
    });
    e.caret = Caret {
        path: vec![0],
        offset: 6,
    };
    e.replace_current_with(";");
    check(&e);

    let mut e = ed(BEFORE_EMPTY);
    assert_eq!(e.replace_all(":", ";", true), 1);
    check(&e);
}

/// Whether every inline content control in `content`, and in the hyperlinks
/// it holds, opens and closes in pairs.
fn content_balances(content: &[Inline]) -> bool {
    let mut depth = 0i32;
    for i in content {
        match i {
            Inline::Raw(r) if crate::hf::is_sdt_open(r) => depth += 1,
            Inline::Raw(r) if crate::hf::is_sdt_close(r) => depth -= 1,
            Inline::Hyperlink(h) if !content_balances(&h.content) => return false,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

#[test]
fn enter_at_the_end_of_a_placeholder_continues_it() {
    let mut e = body_doc();
    e.set_cover_page(0, &[]).unwrap();
    let title = field_para(&e, Placeholder::Title);
    type_over(&mut e, title, "Line1");
    e.insert_newline();
    e.insert_str("Line2");
    assert!(
        inline_controls_balance(&e),
        "{}",
        blocks_to_xml(&e.doc.body)
    );
    let xml = blocks_to_xml(&e.doc.body);
    assert!(
        xml.contains("<w:sdtContent><w:r><w:rPr><w:sz w:val=\"72\"/><w:szCs w:val=\"72\"/></w:rPr><w:t xml:space=\"preserve\">Line2</w:t>"),
        "the new line is in the control, formatted as it: {xml}"
    );
    let mut r = reloaded(&e);
    r.set_cover_page(1, &[]).unwrap();
    assert_eq!(
        text_at(&r, &[field_para(&r, Placeholder::Title)]),
        "Line1\nLine2"
    );
}

#[test]
fn a_paste_of_paragraphs_into_an_emptied_placeholder_is_all_carried() {
    let mut e = body_doc();
    e.set_cover_page(0, &[]).unwrap();
    let title = field_para(&e, Placeholder::Title);
    let len = text_at(&e, &[title]).chars().count();
    e.anchor = Some(Caret {
        path: vec![title],
        offset: 0,
    });
    e.caret = Caret {
        path: vec![title],
        offset: len,
    };
    e.paste(&Clip::from_text("A\nB\nC"));
    assert!(
        inline_controls_balance(&e),
        "{}",
        blocks_to_xml(&e.doc.body)
    );
    e.set_cover_page(1, &[]).unwrap();
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Title)]),
        "A\nB\nC"
    );
}

#[test]
fn enter_after_text_that_follows_a_control_splits_as_before() {
    let mut e = ed(
        "<w:p><w:sdt><w:sdtPr><w:alias w:val=\"Title\"/></w:sdtPr><w:sdtContent>\
         <w:r><w:t>x</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>end</w:t></w:r></w:p>",
    );
    e.caret = Caret {
        path: vec![0],
        offset: 4,
    };
    e.insert_newline();
    let Block::Paragraph(p) = &e.doc.body[1] else {
        panic!()
    };
    assert!(p.content.is_empty(), "{:?}", p.content);
    assert!(inline_controls_balance(&e));
}

#[test]
fn a_split_inside_a_link_keeps_its_control_whole() {
    let mut e = ed(
        "<w:p><w:hyperlink w:anchor=\"top\"><w:sdt><w:sdtPr><w:alias w:val=\"Title\"/>\
         </w:sdtPr><w:sdtContent><w:r><w:t>abcd</w:t></w:r></w:sdtContent></w:sdt></w:hyperlink></w:p>",
    );
    e.caret = Caret {
        path: vec![0],
        offset: 2,
    };
    e.insert_newline();
    for k in 0..2 {
        let Block::Paragraph(p) = &e.doc.body[k] else {
            panic!()
        };
        assert!(
            content_balances(&p.content),
            "{}",
            blocks_to_xml(&e.doc.body)
        );
    }
    let r = reloaded(&e);
    assert_eq!(all_text(&r), all_text(&e));
}

/// A Word cover keeping its title in a text box and its author in a table.
const BOXED_COVER: &str = "<w:sdt><w:sdtPr><w:id w:val=\"5\"/><w:docPartObj>\
    <w:docPartGallery w:val=\"Cover Pages\"/><w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>\
    <w:p><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:sdt><w:sdtPr>\
    <w:alias w:val=\"Title\"/><w:id w:val=\"6\"/><w:showingPlcHdr/></w:sdtPr><w:sdtContent>\
    <w:r><w:t>Boxed Title</w:t></w:r></w:sdtContent></w:sdt></w:p></w:txbxContent></v:textbox>\
    </v:shape></w:pict></w:r></w:p>\
    <w:tbl><w:tblGrid><w:gridCol w:w=\"5000\"/></w:tblGrid><w:tr><w:tc><w:p><w:sdt><w:sdtPr>\
    <w:alias w:val=\"Author\"/><w:id w:val=\"7\"/></w:sdtPr><w:sdtContent><w:r><w:t>Ada</w:t></w:r>\
    </w:sdtContent></w:sdt></w:p></w:tc></w:tr></w:tbl>\
    <w:p><w:r><w:br w:type=\"page\"/></w:r></w:p></w:sdtContent></w:sdt>";

#[test]
fn a_word_cover_with_text_boxes_and_tables_carries_their_text() {
    let mut e = ed(&format!(
        "{BOXED_COVER}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}"
    ));
    e.set_cover_page(0, &[]).unwrap();
    assert_eq!(covers(&e), 1);
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Title)]),
        "Boxed Title"
    );
    assert_eq!(text_at(&e, &[field_para(&e, Placeholder::Author)]), "Ada");
}

fn has_boundaries(p: &Block) -> bool {
    matches!(p, Block::Paragraph(p) if p.content.iter().any(
        |i| matches!(i, Inline::Raw(r) if crate::hf::is_sdt_open(r) || crate::hf::is_sdt_close(r))
    ))
}

/// A date picker named "Date" (a placeholder's name, but not in a cover).
const DATE_LINE: &str = "<w:p><w:r><w:t xml:space=\"preserve\">Date: </w:t></w:r>\
    <w:sdt><w:sdtPr><w:alias w:val=\"Date\"/><w:id w:val=\"8\"/><w:date/></w:sdtPr>\
    <w:sdtContent><w:r><w:t>2026-10-02</w:t></w:r></w:sdtContent></w:sdt></w:p>";

/// A checkbox control after text.
const CHECKBOX_LINE: &str = "<w:p><w:r><w:t xml:space=\"preserve\">I agree </w:t></w:r>\
    <w:sdt><w:sdtPr><w:id w:val=\"9\"/><w14:checkbox><w14:checked w14:val=\"0\"/></w14:checkbox>\
    </w:sdtPr><w:sdtContent><w:r><w:t>☐</w:t></w:r></w:sdtContent></w:sdt></w:p>";

#[test]
fn enter_at_the_end_of_a_line_ending_in_a_control_gives_a_plain_paragraph() {
    for line in [DATE_LINE, CHECKBOX_LINE] {
        let mut e = ed(line);
        let len = text_at(&e, &[0]).chars().count();
        let before = blocks_to_xml(&e.doc.body);
        e.caret = Caret {
            path: vec![0],
            offset: len,
        };
        e.insert_newline();
        let Block::Paragraph(p) = &e.doc.body[1] else {
            panic!()
        };
        assert!(p.content.is_empty(), "{line}: {:?}", p.content);
        // Backspace joins them back as they were.
        e.backspace();
        assert_eq!(blocks_to_xml(&e.doc.body), before, "{line}");
    }
}

#[test]
fn enter_just_after_a_control_mid_line_starts_with_plain_text() {
    let mut e = ed(
        "<w:p><w:sdt><w:sdtPr><w:id w:val=\"3\"/></w:sdtPr><w:sdtContent>\
         <w:r><w:t>x</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>tail</w:t></w:r></w:p>",
    );
    e.caret = Caret {
        path: vec![0],
        offset: 1,
    };
    e.insert_newline();
    let Block::Paragraph(p) = &e.doc.body[1] else {
        panic!()
    };
    assert!(
        matches!(&p.content[..], [Inline::Run(r)] if r.text == "tail"),
        "{:?}",
        p.content
    );
    assert!(inline_controls_balance(&e));
}

#[test]
fn enter_before_a_control_moves_it_down_whole() {
    let mut e = ed(
        "<w:p><w:sdt><w:sdtPr><w:id w:val=\"7\"/><w14:checkbox/></w:sdtPr>\
         <w:sdtContent><w:r><w:t>☐</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>Milk</w:t></w:r></w:p>",
    );
    e.insert_newline();
    assert!(
        !has_boundaries(&e.doc.body[0]),
        "{}",
        blocks_to_xml(&e.doc.body)
    );
    let xml = blocks_to_xml(&e.doc.body[1..2]);
    assert!(
        xml.contains("<w:id w:val=\"7\"/>") && xml.contains("Milk"),
        "{xml}"
    );
    assert!(inline_controls_balance(&e));

    // The same for a cover's title: the control, with its id, goes down.
    let (mut e, title) = typed_title(0);
    let id = |e: &Editor, k: usize| crate::cover::sdt_ids(&blocks_to_xml(&e.doc.body[k..=k]));
    let ids = id(&e, title);
    e.insert_newline();
    assert!(!has_boundaries(&e.doc.body[title]));
    assert_eq!(id(&e, title + 1), ids);
    assert_eq!(text_at(&e, &[title + 1]), "My title");
}

#[test]
fn a_continued_placeholder_drops_the_originals_binding() {
    let mut e = ed(&format!(
        "{}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    let title = field_para(&e, Placeholder::Title);
    e.caret = Caret {
        path: vec![title],
        offset: 6,
    };
    e.insert_newline();
    assert!(inline_controls_balance(&e));
    let first = blocks_to_xml(&e.doc.body[title..=title]);
    let second = blocks_to_xml(&e.doc.body[title + 1..=title + 1]);
    assert!(first.contains("w:showingPlcHdr") && first.contains("w:dataBinding"));
    assert!(
        second.contains("<w:alias w:val=\"Title\"/>")
            && !second.contains("w:showingPlcHdr")
            && !second.contains("w:dataBinding")
            && !second.contains("<w:id "),
        "{second}"
    );
}

/// A checkbox control (id 7) before "Milk", with "ab" before it on the line
/// when `mid` is set.
fn checkbox_doc(mid: bool) -> Editor {
    let before = if mid { "<w:r><w:t>ab</w:t></w:r>" } else { "" };
    ed(&format!(
        "<w:p>{before}<w:sdt><w:sdtPr><w:id w:val=\"7\"/><w14:checkbox/></w:sdtPr>\
         <w:sdtContent><w:r><w:t>☐</w:t></w:r></w:sdtContent></w:sdt><w:r><w:t>Milk</w:t></w:r></w:p>"
    ))
}

#[test]
fn blank_page_and_a_paste_at_a_controls_start_move_it_down_whole() {
    for mid in [false, true] {
        let at = if mid { 2 } else { 0 };
        for blank in [true, false] {
            let mut e = checkbox_doc(mid);
            e.caret = Caret {
                path: vec![0],
                offset: at,
            };
            if blank {
                e.insert_blank_page();
            } else {
                e.paste(&Clip::from_text("X\nY"));
            }
            assert!(inline_controls_balance(&e));
            let xml = blocks_to_xml(&e.doc.body);
            assert_eq!(xml.matches("<w:sdtContent>").count(), 1, "{xml}");
            let last = e
                .doc
                .body
                .iter()
                .rposition(|b| matches!(b, Block::Paragraph(_)))
                .unwrap();
            let last_xml = blocks_to_xml(&e.doc.body[last..=last]);
            assert!(
                last_xml.contains("<w:id w:val=\"7\"/>") && last_xml.contains("Milk"),
                "mid {mid}, blank {blank}: {xml}"
            );
        }
    }
}

fn plain_doc_control(offset: usize) -> (Editor, String) {
    let e = ed(
        "<w:p><w:r><w:t xml:space=\"preserve\">Say </w:t></w:r><w:sdt><w:sdtPr>\
         <w:id w:val=\"4\"/><w:dataBinding w:xpath=\"/a\" w:storeItemID=\"{X}\"/></w:sdtPr>\
         <w:sdtContent><w:r><w:t>abcd</w:t></w:r></w:sdtContent></w:sdt></w:p>",
    );
    let before = blocks_to_xml(&e.doc.body);
    let mut e = e;
    e.caret = Caret {
        path: vec![0],
        offset,
    };
    (e, before)
}

#[test]
fn enter_then_backspace_or_delete_inside_a_control_is_undone_exactly() {
    for delete in [false, true] {
        let (mut e, before) = plain_doc_control(6);
        e.insert_newline();
        assert_eq!(e.doc.body.len(), 2);
        if delete {
            e.caret = Caret {
                path: vec![0],
                offset: 6,
            };
            e.delete_forward();
        } else {
            e.backspace();
        }
        // Enter splits the run and the join keeps the two halves as two runs,
        // as it does in any paragraph; the control is whole again, with its id
        // and binding.
        let after =
            blocks_to_xml(&e.doc.body).replace("</w:t></w:r><w:r><w:t xml:space=\"preserve\">", "");
        assert_eq!(after, before, "delete {delete}");
    }
}

#[test]
fn enter_then_backspace_in_a_cover_title_keeps_one_placeholder() {
    for offset in [2, 8] {
        let (mut e, title) = typed_title(offset);
        e.insert_newline();
        e.backspace();
        let Block::Paragraph(p) = &e.doc.body[title] else {
            panic!()
        };
        let opens = p
            .content
            .iter()
            .filter(|i| matches!(i, Inline::Raw(r) if crate::hf::is_sdt_open(r)))
            .count();
        assert_eq!(opens, 1, "offset {offset}: {:?}", p.content);
        // Typing at the end goes into the original placeholder.
        e.caret.offset = text_at(&e, &[title]).chars().count();
        e.insert_str("!");
        e.set_cover_page(1, &[]).unwrap();
        assert_eq!(
            text_at(&e, &[field_para(&e, Placeholder::Title)]),
            "My title!",
            "offset {offset}"
        );
    }
}

#[test]
fn a_paste_at_the_end_of_a_typed_placeholder_is_its_text() {
    let (mut e, _) = typed_title(8);
    e.paste(&Clip::from_text(" 2026"));
    e.set_cover_page(1, &[]).unwrap();
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Title)]),
        "My title 2026"
    );

    let (mut e, _) = typed_title(8);
    e.paste(&Clip::from_text("A\nB"));
    assert!(inline_controls_balance(&e));
    e.set_cover_page(1, &[]).unwrap();
    assert_eq!(
        text_at(&e, &[field_para(&e, Placeholder::Title)]),
        "My titleA\nB"
    );

    // An ordinary control keeps its paste placement: after its end.
    let (mut e, _) = plain_doc_control(8);
    e.paste(&Clip::from_text("!"));
    let xml = blocks_to_xml(&e.doc.body);
    assert!(xml.contains("</w:sdtContent></w:sdt><w:r>"), "{xml}");
}

/// Two paragraphs each holding one control, built from the two sdtPr bodies
/// (each with its own w:id).
fn two_controls(first: &str, second: &str) -> Editor {
    ed(&format!(
        "<w:p><w:sdt><w:sdtPr>{first}</w:sdtPr><w:sdtContent><w:r><w:t>John</w:t></w:r>\
         </w:sdtContent></w:sdt></w:p><w:p><w:sdt><w:sdtPr>{second}</w:sdtPr><w:sdtContent>\
         <w:r><w:t>Smith</w:t></w:r></w:sdtContent></w:sdt></w:p>"
    ))
}

#[test]
fn joining_two_separate_controls_keeps_them_two() {
    let pairs = [
        // Word's default plain-text controls: alike but for their ids.
        ("<w:id w:val=\"1\"/>", "<w:id w:val=\"2\"/>"),
        // Bound to different properties.
        (
            "<w:id w:val=\"1\"/><w:dataBinding w:xpath=\"/a\" w:storeItemID=\"{X}\"/>",
            "<w:id w:val=\"2\"/><w:dataBinding w:xpath=\"/b\" w:storeItemID=\"{X}\"/>",
        ),
        // Two checkboxes.
        (
            "<w:id w:val=\"1\"/><w14:checkbox/>",
            "<w:id w:val=\"2\"/><w14:checkbox/>",
        ),
    ];
    for (first, second) in pairs {
        for how in ["backspace", "delete", "selection"] {
            let mut e = two_controls(first, second);
            match how {
                "backspace" => {
                    e.caret = Caret {
                        path: vec![1],
                        offset: 0,
                    };
                    e.backspace();
                }
                "delete" => {
                    e.caret = Caret {
                        path: vec![0],
                        offset: 4,
                    };
                    e.delete_forward();
                }
                _ => {
                    e.anchor = Some(Caret {
                        path: vec![0],
                        offset: 4,
                    });
                    e.caret = Caret {
                        path: vec![1],
                        offset: 0,
                    };
                    e.delete_selection();
                }
            }
            assert_eq!(e.doc.body.len(), 1, "{how}");
            let xml = blocks_to_xml(&e.doc.body);
            assert_eq!(xml.matches("<w:sdtContent>").count(), 2, "{how}: {xml}");
            assert_eq!(crate::cover::sdt_ids(&xml), vec![1, 2], "{how}: {xml}");
            assert!(xml.contains(first) && xml.contains(second), "{how}: {xml}");
        }
    }
}

#[test]
fn typing_over_a_word_prompt_clears_showing_placeholder() {
    let mut e = ed(&format!(
        "{}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    let title = field_para(&e, Placeholder::Title);
    let flagged = |e: &Editor| blocks_to_xml(&e.doc.body[title..=title]).contains("showingPlcHdr");
    assert!(flagged(&e));
    type_over(&mut e, title, "Mine");
    assert_eq!(text_at(&e, &[title]), "Mine");
    assert!(
        !flagged(&e),
        "{}",
        blocks_to_xml(&e.doc.body[title..=title])
    );
    let xml = blocks_to_xml(&e.doc.body[title..=title]);
    let inside = &xml[xml.find("<w:sdtContent>").unwrap()..xml.find("</w:sdtContent>").unwrap()];
    assert!(inside.contains("Mine"), "{xml}");
    // Undo brings the prompt back, flag and all.
    while e.undo() {}
    assert!(flagged(&e));
    assert_eq!(text_at(&e, &[title]), "Annual Report");

    // A paste over it clears the flag too.
    let mut e = ed(&format!(
        "{}<w:p><w:r><w:t>Body text</w:t></w:r></w:p>{SECT}",
        crate::cover::tests::WORD_COVER
    ));
    let len = text_at(&e, &[title]).chars().count();
    e.anchor = Some(Caret {
        path: vec![title],
        offset: 0,
    });
    e.caret = Caret {
        path: vec![title],
        offset: len,
    };
    e.paste(&Clip::from_text("Pasted"));
    assert!(!flagged(&e));
}
