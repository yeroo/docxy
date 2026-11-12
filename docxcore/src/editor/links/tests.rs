use super::*;
use crate::load::{parse_document_xml, parse_rels_xml};
use crate::model::{Block, Document, Paragraph};
use crate::package::{load_package, new_package, save_package};
use crate::serialize::document_to_xml;
use crate::xml::{Event, XmlParser};

fn run(text: &str, props: RunProps) -> Inline {
    Inline::Run(Run {
        text: text.into(),
        props,
    })
}

fn plain(text: &str) -> Inline {
    run(text, RunProps::default())
}

fn bold() -> RunProps {
    RunProps {
        bold: true,
        ..RunProps::default()
    }
}

fn link(url: &str, runs: Vec<Inline>) -> Inline {
    Inline::Hyperlink(Hyperlink {
        target: Some(url.into()),
        runs: runs
            .into_iter()
            .map(|i| match i {
                Inline::Run(r) => r,
                other => panic!("not a run: {other:?}"),
            })
            .collect(),
        ..Hyperlink::default()
    })
}

fn editor(paras: Vec<Vec<Inline>>) -> Editor {
    let mut ed = Editor::new(Document {
        body: paras
            .into_iter()
            .map(|content| {
                Block::Paragraph(Paragraph {
                    content,
                    ..Paragraph::default()
                })
            })
            .collect(),
    });
    ed.caret = Caret::at(vec![0], 0);
    ed
}

/// An editor over a document loaded from `body` (paragraphs' XML), its
/// `rId1` an external link to `https://old.example/`.
fn loaded(body: &str) -> Editor {
    let rels = parse_rels_xml(
        "<Relationships><Relationship Id=\"rId1\" Target=\"https://old.example/\" \
         TargetMode=\"External\"/></Relationships>",
    );
    let doc = parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &rels,
    );
    let mut ed = Editor::new(doc);
    ed.caret = Caret::at(vec![0], 0);
    ed
}

fn select(ed: &mut Editor, a: (usize, usize), b: (usize, usize)) {
    ed.anchor = Some(Caret::at(vec![a.0], a.1));
    ed.caret = Caret::at(vec![b.0], b.1);
}

fn content(ed: &Editor, i: usize) -> &[Inline] {
    match &ed.doc.body[i] {
        Block::Paragraph(p) => &p.content,
        other => panic!("not a paragraph: {other:?}"),
    }
}

fn links(ed: &Editor, i: usize) -> Vec<&Hyperlink> {
    content(ed, i)
        .iter()
        .filter_map(|inline| match inline {
            Inline::Hyperlink(h) => Some(h),
            _ => None,
        })
        .collect()
}

fn text(ed: &Editor, i: usize) -> String {
    match &ed.doc.body[i] {
        Block::Paragraph(p) => p.plain_text(),
        _ => String::new(),
    }
}

/// `xml` is balanced element by element, with every prefix it uses on an
/// element or attribute declared on it or an ancestor (`w`, `r` and `xml`
/// aside, which the part root declares).
fn assert_well_formed(xml: &str) {
    let mut p = XmlParser::new(xml);
    let mut stack: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        match p.next() {
            Event::Start => {
                let declared: Vec<String> = p
                    .attrs()
                    .iter()
                    .filter_map(|a| a.name.strip_prefix("xmlns:").map(str::to_string))
                    .collect();
                let bound = |prefix: &str| {
                    ["w", "r", "xml", "xmlns"].contains(&prefix)
                        || declared.iter().any(|d| d == prefix)
                        || stack.iter().any(|(_, ds)| ds.iter().any(|d| d == prefix))
                };
                let names = std::iter::once(p.name()).chain(p.attrs().iter().map(|a| a.name));
                for name in names {
                    if let Some((prefix, _)) = name.split_once(':') {
                        assert!(bound(prefix), "unbound prefix {prefix}: {xml}");
                    }
                }
                stack.push((p.name().to_string(), declared));
            }
            Event::End => {
                let open = stack.pop().map(|(name, _)| name);
                assert_eq!(open.as_deref(), Some(p.name()), "malformed: {xml}");
            }
            Event::Text => {}
            Event::Eof => break,
        }
    }
    assert!(stack.is_empty(), "unclosed: {xml}");
}

#[test]
fn insert_wraps_a_bold_selection_keeping_its_formatting_1952() {
    let mut ed = editor(vec![vec![
        plain("Say "),
        run("Beta", bold()),
        plain(" now"),
    ]]);
    select(&mut ed, (0, 4), (0, 8));
    let before = ed.doc.clone();
    assert!(ed.insert_hyperlink("Beta", "https://b.example/"));
    let c = content(&ed, 0);
    assert_eq!(c.len(), 3, "{c:?}");
    let Inline::Hyperlink(h) = &c[1] else {
        panic!("a link: {c:?}");
    };
    assert_eq!(h.target.as_deref(), Some("https://b.example/"));
    assert_eq!(h.runs.len(), 1);
    let r = &h.runs[0];
    assert_eq!(r.text, "Beta");
    assert!(r.props.bold && r.props.underline);
    assert_eq!(r.props.color.as_deref(), Some(LINK_COLOR));
    assert_eq!(text(&ed, 0), "Say Beta now");
    assert_eq!((ed.caret.offset, ed.anchor.clone()), (8, None));
    assert_eq!(ed.undo_names(), ["Hyperlink"]);
    // One undo takes it back, the selection with it.
    assert!(ed.undo());
    assert_eq!(ed.doc, before);
    assert_eq!(ed.anchor, Some(Caret::at(vec![0], 4)));
}

#[test]
fn insert_wraps_part_of_a_run_and_keeps_a_colour_of_its_own_1952() {
    let red = RunProps {
        color: Some("FF0000".into()),
        ..RunProps::default()
    };
    let mut ed = editor(vec![vec![run("Hello world", red)]]);
    select(&mut ed, (0, 11), (0, 6));
    assert!(ed.insert_hyperlink("world", "https://w.example/"));
    let c = content(&ed, 0);
    assert_eq!(c.len(), 2, "{c:?}");
    let Inline::Hyperlink(h) = &c[1] else {
        panic!("a link: {c:?}");
    };
    assert_eq!(h.runs[0].text, "world");
    assert_eq!(h.runs[0].props.color.as_deref(), Some("FF0000"));
    assert!(h.runs[0].props.underline);
    assert_eq!(ed.caret.offset, 11);
}

#[test]
fn insert_with_another_text_replaces_the_selection_1952() {
    let mut ed = editor(vec![vec![
        plain("Say "),
        run("Beta", bold()),
        plain(" now"),
    ]]);
    select(&mut ed, (0, 4), (0, 8));
    assert!(ed.insert_hyperlink("Click", "https://c.example/"));
    assert_eq!(text(&ed, 0), "Say Click now");
    let h = links(&ed, 0)[0];
    assert_eq!(h.runs.len(), 1);
    // Typing at the caret (after "Say ") is plain: the link look only.
    let r = &h.runs[0];
    assert_eq!(r.text, "Click");
    assert!(!r.props.bold && r.props.underline);
    assert_eq!(r.props.color.as_deref(), Some(LINK_COLOR));
    assert_eq!((ed.caret.offset, ed.anchor.clone()), (9, None));
    assert_eq!(ed.undo_names(), ["Hyperlink"]);
    assert!(ed.undo());
    assert_eq!(text(&ed, 0), "Say Beta now");
}

#[test]
fn insert_at_a_caret_with_no_text_shows_the_url_and_hash_makes_an_anchor_1952() {
    let mut ed = editor(vec![vec![plain("abc")]]);
    ed.caret.offset = 3;
    assert!(ed.insert_hyperlink("  ", "  https://u.example/ "));
    assert_eq!(text(&ed, 0), "abchttps://u.example/");
    assert_eq!(
        links(&ed, 0)[0].target.as_deref(),
        Some("https://u.example/")
    );
    assert_eq!(ed.caret.offset, 21);

    let mut ed = editor(vec![vec![plain("abc")]]);
    assert!(ed.insert_hyperlink("Top", "#top"));
    let h = links(&ed, 0)[0];
    assert_eq!(
        (h.target.as_deref(), h.anchor.as_deref()),
        (None, Some("top"))
    );
    assert_eq!(
        ed.caret_link(),
        Some(CaretLink {
            text: "Top".into(),
            url: "#top".into()
        })
    );
}

#[test]
fn insert_inside_a_link_lands_between_its_halves_1952() {
    let mut ed = editor(vec![vec![link(
        "https://a.example/",
        vec![plain("docxy link")],
    )]]);
    ed.caret.offset = 5;
    assert!(ed.insert_hyperlink("X", "https://n.example/"));
    let c = content(&ed, 0);
    let targets: Vec<_> = c
        .iter()
        .map(|i| match i {
            Inline::Hyperlink(h) => {
                assert!(
                    h.content.iter().all(|i| !matches!(i, Inline::Hyperlink(_))),
                    "nested: {h:?}"
                );
                h.target.clone().unwrap_or_default()
            }
            other => panic!("not a link: {other:?}"),
        })
        .collect();
    assert_eq!(
        targets,
        [
            "https://a.example/",
            "https://n.example/",
            "https://a.example/"
        ]
    );
    assert_eq!(text(&ed, 0), "docxyX link");
}

#[test]
fn insert_over_two_paragraphs_replaces_them_with_one_line_1952() {
    let mut ed = editor(vec![vec![plain("ab")], vec![plain("cd")]]);
    select(&mut ed, (0, 1), (1, 1));
    // What a host prefills from the selection, passed back.
    let shown = ed.selection_text();
    assert_eq!(shown, "b\nc");
    assert!(ed.insert_hyperlink(&shown, "https://p.example/"));
    assert_eq!(ed.doc.body.len(), 1);
    assert_eq!(text(&ed, 0), "ab cd");
    assert_eq!(links(&ed, 0)[0].runs[0].text, "b c");
    assert_eq!(ed.undo_names(), ["Hyperlink"]);
}

#[test]
fn link_text_is_one_line_a_document_can_hold_1952() {
    assert_eq!(
        link_text("a\r\nb\rc\nd\te\u{0B}f\u{1}g\u{FFFE}h\u{FFFF}i\u{FFFC}j"),
        "a b c d e fghij"
    );
    let mut ed = editor(vec![vec![plain("x")]]);
    assert!(ed.insert_hyperlink("one\u{7}\ntwo", "https://t.example/"));
    assert_eq!(links(&ed, 0)[0].runs[0].text, "one two");
}

#[test]
fn a_url_that_is_empty_bare_or_holds_a_control_is_refused_1952() {
    for url in [
        "",
        "   ",
        "#",
        " # ",
        "https://a\u{7}b/",
        "https://a\nb/",
        "x\u{FFFF}",
    ] {
        let mut ed = editor(vec![vec![plain("Say Beta")]]);
        select(&mut ed, (0, 4), (0, 8));
        let before = ed.doc.clone();
        assert!(!ed.insert_hyperlink("Beta", url), "{url:?}");
        assert_eq!(ed.doc, before);
        assert!(ed.undo_names().is_empty());
        assert_eq!(
            ed.anchor,
            Some(Caret::at(vec![0], 4)),
            "the selection stays"
        );
    }
    let mut ed = editor(vec![vec![link("https://a.example/", vec![plain("ab")])]]);
    ed.caret.offset = 1;
    assert!(!ed.edit_hyperlink("ab", "#"));
    assert!(ed.undo_names().is_empty());
}

#[test]
fn a_wrap_takes_runs_and_whole_markers_only_1952() {
    let bookmark = Inline::Raw("<w:bookmarkStart w:id=\"1\" w:name=\"b\"/>".into());
    let close = Inline::Raw(crate::load::SDT_BLOCK_CLOSE.into());
    let field = Inline::Field {
        text: "F".into(),
        raw: String::new(),
    };
    let tab = Inline::Tab(RunProps::default());
    let c = vec![plain("ab"), bookmark.clone(), plain("cd")];
    assert!(wrappable(&c, 1, 3));
    let c = vec![plain("ab"), close.clone(), plain("cd")];
    assert!(!wrappable(&c, 1, 3));
    // At an edge a zero-width inline stays outside the link.
    assert!(wrappable(&c, 0, 2));
    assert!(wrappable(&c, 2, 4));
    let c = vec![plain("ab"), field, plain("cd")];
    assert!(!wrappable(&c, 1, 4));
    let c = vec![plain("ab"), tab, plain("cd")];
    assert!(!wrappable(&c, 1, 4));
    assert!(!whole_marker(&Inline::Raw("<w:sdt><w:sdtContent>".into())));
    assert!(!whole_marker(&Inline::Raw(
        "<w:proofErr w:type=\"a\"/><w:r/>".into()
    )));
    assert!(whole_marker(&Inline::Raw(
        "<w:commentRangeEnd w:id=\"3\"/>".into()
    )));
}

#[test]
fn a_bookmark_inside_the_selection_stays_inside_the_new_link_1952() {
    let mark = "<w:bookmarkStart w:id=\"1\" w:name=\"b\"/>";
    let mut ed = editor(vec![vec![
        plain("ab"),
        Inline::Raw(mark.into()),
        plain("cd"),
        Inline::Raw("<w:bookmarkEnd w:id=\"1\"/>".into()),
    ]]);
    select(&mut ed, (0, 1), (0, 4));
    assert!(ed.insert_hyperlink("bcd", "https://m.example/"));
    let c = content(&ed, 0);
    let Inline::Hyperlink(h) = &c[1] else {
        panic!("a link: {c:?}");
    };
    assert!(h.runs.is_empty());
    assert_eq!(h.content.len(), 3, "{h:?}");
    assert_eq!(h.content[1], Inline::Raw(mark.into()));
    // The end at the selection's end stays after the link.
    assert!(matches!(&c[2], Inline::Raw(r) if r.starts_with("<w:bookmarkEnd")));
    assert_eq!(text(&ed, 0), "abcd");
    assert_eq!(ed.caret_link().map(|l| l.text), Some("bcd".into()));
    assert_well_formed(&document_to_xml(&ed.doc));
}

#[test]
fn a_selection_across_a_content_control_edge_is_replaced_and_saves_1952() {
    let mut ed = loaded(
        "<w:p><w:sdt><w:sdtPr/><w:sdtContent><w:r><w:t>ab</w:t></w:r></w:sdtContent></w:sdt>\
         <w:r><w:t>cd</w:t></w:r></w:p>",
    );
    select(&mut ed, (0, 1), (0, 3));
    let before = content(&ed, 0).len();
    assert_eq!(ed.selection_text(), "bc");
    assert!(ed.insert_hyperlink("bc", "https://s.example/"));
    assert_eq!(text(&ed, 0), "abcd");
    assert_eq!(links(&ed, 0).len(), 1);
    assert!(content(&ed, 0).len() > before);
    let xml = document_to_xml(&ed.doc);
    assert_well_formed(&xml);
    let back = parse_document_xml(&xml, &parse_rels_xml("<Relationships/>"));
    assert_eq!(back.body[0].plain_text(), "abcd");
}

#[test]
fn while_tracking_a_wrap_is_a_recorded_insertion_instead_1952() {
    let mut ed = editor(vec![vec![
        plain("Say "),
        run("Beta", bold()),
        plain(" now"),
    ]]);
    ed.set_track_changes(Some(crate::editor::TrackAuthor {
        author: "A".into(),
        clock: || "2026-01-01T00:00:00Z".into(),
    }));
    select(&mut ed, (0, 4), (0, 8));
    assert!(ed.insert_hyperlink("Beta", "https://b.example/"));
    let h = links(&ed, 0)
        .into_iter()
        .find(|h| h.target.as_deref() == Some("https://b.example/"))
        .expect("the new link");
    let inserted = h.runs.iter().any(|r| r.props.tracked_insert.is_some())
        || h.content.iter().any(|i| match i {
            Inline::Run(r) => r.props.tracked_insert.is_some(),
            _ => false,
        });
    assert!(inserted, "{h:?}");
    assert_eq!(ed.undo_names(), ["Hyperlink"]);
}

#[test]
fn the_link_at_the_caret_is_the_one_typing_goes_into_1952() {
    let mut ed = editor(vec![vec![
        plain("ab"),
        link("https://l.example/", vec![plain("cd")]),
        plain("ef"),
    ]]);
    let at = |ed: &mut Editor, o: usize| {
        ed.anchor = None;
        ed.caret.offset = o;
        ed.caret_link().map(|l| l.url)
    };
    // At its start the caret belongs to "ab"; at its end, to the link.
    assert_eq!(at(&mut ed, 2), None);
    assert_eq!(at(&mut ed, 3).as_deref(), Some("https://l.example/"));
    assert_eq!(at(&mut ed, 4).as_deref(), Some("https://l.example/"));
    assert_eq!(at(&mut ed, 5), None);
    // A selection inside the link (its whole text too) is on it; one
    // crossing its edge is not.
    for (a, b, on) in [(2, 4, true), (3, 4, true), (1, 3, false), (3, 5, false)] {
        select(&mut ed, (0, a), (0, b));
        assert_eq!(ed.caret_link().is_some(), on, "{a}..{b}");
    }

    // A link that opens the paragraph holds a caret at 0.
    let mut ed = editor(vec![vec![
        link("https://l.example/", vec![plain("cd")]),
        plain("ef"),
    ]]);
    assert_eq!(at(&mut ed, 0).as_deref(), Some("https://l.example/"));
}

#[test]
fn edit_changes_a_loaded_links_target_and_keeps_its_attributes_1952() {
    let mut ed = loaded(
        "<w:p><w:hyperlink r:id=\"rId1\" w:tooltip=\"tip\" w:history=\"1\">\
         <w:r><w:rPr><w:b/></w:rPr><w:t>old text</w:t></w:r></w:hyperlink></w:p>",
    );
    ed.caret.offset = 3;
    assert_eq!(
        ed.caret_link(),
        Some(CaretLink {
            text: "old text".into(),
            url: "https://old.example/".into()
        })
    );
    assert!(ed.edit_hyperlink("old text", "https://new.example/"));
    let h = links(&ed, 0)[0].clone();
    assert_eq!(h.target.as_deref(), Some("https://new.example/"));
    assert_eq!(h.rel_id, None);
    let raw = h.raw.as_deref().expect("the opener kept");
    assert!(
        raw.contains("w:tooltip=\"tip\"") && !raw.contains("r:id"),
        "{raw}"
    );
    assert_eq!(h.runs[0].text, "old text");
    assert!(h.runs[0].props.bold);
    assert_eq!((ed.caret.offset, ed.anchor.clone()), (8, None));
    assert_eq!(ed.undo_names(), ["Edit Hyperlink"]);

    // Saved and loaded again, the new target is the link's, with its
    // tooltip.
    let saved = save_package(&new_package(ed.export_doc().into_owned()));
    let pkg = load_package(&saved).unwrap();
    let xml = pkg.part_text("word/document.xml").unwrap();
    assert!(xml.contains("w:tooltip=\"tip\""), "{xml}");
    let Block::Paragraph(p) = &pkg.document.body[0] else {
        panic!("a paragraph");
    };
    let Inline::Hyperlink(back) = &p.content[0] else {
        panic!("a link: {:?}", p.content);
    };
    assert_eq!(back.target.as_deref(), Some("https://new.example/"));

    // A new text replaces the runs, in the first run's formatting.
    assert!(ed.edit_hyperlink("fresh", "https://new.example/"));
    let h = links(&ed, 0)[0];
    assert_eq!(h.runs.len(), 1);
    assert_eq!(h.runs[0].text, "fresh");
    assert!(h.runs[0].props.bold);
    assert_eq!(ed.caret.offset, 5);
    assert_eq!(ed.undo_names(), ["Edit Hyperlink", "Edit Hyperlink"]);
    assert!(ed.undo());
    assert_eq!(text(&ed, 0), "old text");
}

#[test]
fn edit_to_a_bookmark_writes_an_anchor_and_no_id_1952() {
    let mut ed = loaded(
        "<w:p><w:hyperlink r:id=\"rId1\" w:tooltip=\"tip\"><w:r><w:t>go</w:t></w:r></w:hyperlink></w:p>",
    );
    ed.caret.offset = 1;
    assert!(ed.edit_hyperlink("", "#here"));
    let h = links(&ed, 0)[0];
    assert_eq!(
        (h.target.as_deref(), h.anchor.as_deref()),
        (None, Some("here"))
    );
    let saved = save_package(&new_package(ed.export_doc().into_owned()));
    let pkg = load_package(&saved).unwrap();
    let xml = pkg.part_text("word/document.xml").unwrap();
    let at = xml.find("<w:hyperlink").expect("the link");
    let opener = &xml[at..at + xml[at..].find('>').unwrap()];
    assert!(
        opener.contains("w:anchor=\"here\"") && !opener.contains("r:id"),
        "{opener}"
    );
    let Block::Paragraph(p) = &pkg.document.body[0] else {
        panic!("a paragraph");
    };
    assert!(
        matches!(&p.content[0], Inline::Hyperlink(h) if h.anchor.as_deref() == Some("here")),
        "{:?}",
        p.content
    );
}

#[test]
fn edit_with_the_same_text_and_url_changes_nothing_1952() {
    let mut ed = loaded(
        "<w:p><w:hyperlink r:id=\"rId1\" w:tooltip=\"tip\"><w:r><w:t>go</w:t></w:r></w:hyperlink></w:p>",
    );
    ed.caret.offset = 1;
    let before = ed.doc.clone();
    assert!(ed.edit_hyperlink("go", " https://old.example/ "));
    assert!(ed.edit_hyperlink("", "https://old.example/"));
    assert_eq!(ed.doc, before);
    assert!(ed.undo_names().is_empty());
}

#[test]
fn edit_and_remove_off_a_link_do_nothing_1952() {
    let mut ed = editor(vec![vec![
        plain("ab"),
        link("https://l.example/", vec![plain("cd")]),
    ]]);
    ed.caret.offset = 1;
    let before = ed.doc.clone();
    assert!(!ed.edit_hyperlink("x", "https://x.example/"));
    assert!(!ed.remove_hyperlink());
    assert_eq!(ed.doc, before);
    assert!(ed.undo_names().is_empty());
}

#[test]
fn remove_unwraps_the_link_and_takes_its_look_off_1952() {
    let look = RunProps {
        bold: true,
        underline: true,
        color: Some("0563c1".into()),
        ..RunProps::default()
    };
    let styled = RunProps {
        style_id: Some("Hyperlink".into()),
        ..RunProps::default()
    };
    let red = RunProps {
        underline: true,
        color: Some("FF0000".into()),
        ..RunProps::default()
    };
    let mut ed = editor(vec![vec![
        plain("x"),
        link(
            "https://l.example/",
            vec![run("Be", look), run("t", styled), run("a", red.clone())],
        ),
        plain("y"),
    ]]);
    select(&mut ed, (0, 2), (0, 3));
    let before = ed.doc.clone();
    assert!(ed.remove_hyperlink());
    let c = content(&ed, 0);
    assert!(links(&ed, 0).is_empty());
    assert_eq!(text(&ed, 0), "xBetay");
    let Inline::Run(be) = &c[1] else {
        panic!("a run: {c:?}");
    };
    assert!(be.props.bold && !be.props.underline && be.props.color.is_none());
    let Inline::Run(t) = &c[2] else {
        panic!("a run: {c:?}");
    };
    assert_eq!(t.props.style_id, None);
    let Inline::Run(a) = &c[3] else {
        panic!("a run: {c:?}");
    };
    assert_eq!(a.props, red, "another colour is the text's own");
    // The offsets did not move: the selection stays.
    assert_eq!(ed.anchor, Some(Caret::at(vec![0], 2)));
    assert_eq!(ed.caret.offset, 3);
    assert_eq!(ed.undo_names(), ["Remove Hyperlink"]);
    assert!(ed.undo());
    assert_eq!(ed.doc, before);
}

#[test]
fn remove_splices_a_complex_links_content_1952() {
    let mut ed = loaded(
        "<w:p><w:r><w:t>a</w:t></w:r><w:hyperlink r:id=\"rId1\"><w:r><w:t>b</w:t></w:r>\
         <w:bookmarkStart w:id=\"1\" w:name=\"m\"/><w:r><w:t>c</w:t></w:r></w:hyperlink></w:p>",
    );
    assert!(!links(&ed, 0)[0].content.is_empty(), "a complex link");
    ed.caret.offset = 2;
    assert!(ed.remove_hyperlink());
    assert!(links(&ed, 0).is_empty());
    assert_eq!(text(&ed, 0), "abc");
    assert!(
        content(&ed, 0)
            .iter()
            .any(|i| matches!(i, Inline::Raw(r) if r.contains("bookmarkStart")))
    );
    assert_well_formed(&document_to_xml(&ed.doc));
}

/// A link whose smart tag uses a prefix declared on its paragraph (the
/// shape `load` keeps bound on the link's own opener, #1063), edited and
/// removed, saves well-formed XML that keeps its text.
#[test]
fn a_link_holding_a_prefixed_smart_tag_saves_after_an_edit_or_a_remove_1952() {
    let body = "<w:p xmlns:x=\"urn:x\"><w:hyperlink r:id=\"rId1\">\
                <w:smartTag x:a=\"1\" w:element=\"e\"><w:r><w:t>Tag</w:t></w:r></w:smartTag>\
                </w:hyperlink></w:p>";
    let check = |ed: &Editor| {
        let xml = document_to_xml(&ed.doc);
        assert_well_formed(&xml);
        let back = parse_document_xml(&xml, &parse_rels_xml("<Relationships/>"));
        assert_eq!(back.body[0].plain_text(), "Tag", "{xml}");
    };
    let mut ed = loaded(body);
    ed.caret.offset = 1;
    assert!(ed.edit_hyperlink("Tag", "https://n.example/"));
    check(&ed);
    let mut ed = loaded(body);
    ed.caret.offset = 1;
    assert!(ed.remove_hyperlink());
    assert!(links(&ed, 0).is_empty());
    check(&ed);
}

#[test]
fn an_inserted_link_keeps_its_url_through_a_save_1952() {
    let mut ed = editor(vec![vec![plain("Say Beta")]]);
    select(&mut ed, (0, 4), (0, 8));
    assert!(ed.insert_hyperlink("Beta", "https://b.example/?a=1&b=2"));
    let saved = save_package(&new_package(ed.export_doc().into_owned()));
    let pkg = load_package(&saved).unwrap();
    let Block::Paragraph(p) = &pkg.document.body[0] else {
        panic!("a paragraph");
    };
    let h = p
        .content
        .iter()
        .find_map(|i| match i {
            Inline::Hyperlink(h) => Some(h),
            _ => None,
        })
        .expect("the link");
    assert_eq!(h.target.as_deref(), Some("https://b.example/?a=1&b=2"));
    assert!(h.rel_id.is_some());
}
