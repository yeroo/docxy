use super::*;

const BEGIN: &str = "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>";
const SEP: &str = "<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>";
const END: &str = "<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
const TOC_INSTR: &str =
    "<w:r><w:instrText xml:space=\"preserve\"> TOC \\o \"1-3\" \\h \\z \\u </w:instrText></w:r>";

fn instr(text: &str) -> String {
    format!("<w:r><w:instrText xml:space=\"preserve\"> {text} </w:instrText></w:r>")
}

/// A TOC entry: a link to its heading, the title, a tab and the page.
fn entry(title: &str, page: &str) -> String {
    format!(
        "<w:hyperlink w:anchor=\"_Toc{page}\"><w:r><w:t>{title}</w:t></w:r>\
         <w:r><w:tab/></w:r><w:r><w:t>{page}</w:t></w:r></w:hyperlink>"
    )
}

fn doc(paras: &[String]) -> Editor {
    let body: String = paras.iter().map(|p| format!("<w:p>{p}</w:p>")).collect();
    Editor::new(crate::load::parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &crate::load::Relationships::default(),
    ))
}

/// `Before`, then a TOC spanning paragraphs (its first entry led by `lead`),
/// then `After`. `end_alone`: the end run is a paragraph of its own after the
/// entries, else it closes the last entry's paragraph. Returns the editor and
/// the end's caret position e; b is [`b_of`]`(lead)`.
fn toc(lead: &str, end_alone: bool) -> (Editor, Caret) {
    let lead_run = if lead.is_empty() {
        String::new()
    } else {
        format!("<w:r><w:t xml:space=\"preserve\">{lead}</w:t></w:r>")
    };
    let mut paras = vec![
        "<w:r><w:t>Before</w:t></w:r>".to_string(),
        format!("{lead_run}{BEGIN}{TOC_INSTR}{SEP}{}", entry("Intro", "1")),
    ];
    let e = if end_alone {
        paras.push(entry("Body", "2"));
        paras.push(END.to_string());
        Caret::at(vec![3], 0)
    } else {
        paras.push(format!("{}{END}", entry("Body", "2")));
        Caret::at(vec![2], 6)
    };
    paras.push("<w:r><w:t>After</w:t></w:r>".to_string());
    (doc(&paras), e)
}

fn b_of(lead: &str) -> Caret {
    Caret::at(vec![1], lead.chars().count())
}

fn texts(ed: &Editor) -> Vec<String> {
    ed.doc
        .body
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(editor_text(&p.content)),
            _ => None,
        })
        .collect()
}

fn saved(ed: &Editor) -> String {
    crate::serialize::document_to_xml(&ed.doc)
}

/// The saved document holds no field code at all.
fn assert_no_field(ed: &Editor) {
    let xml = saved(ed);
    for gone in ["fldChar", "instrText"] {
        assert!(!xml.contains(gone), "{gone} left behind: {xml}");
    }
}

/// Each kind of field marker is in the saved document `n` times.
fn assert_markers(ed: &Editor, n: usize) {
    let xml = saved(ed);
    for kind in ["begin", "separate", "end"] {
        let marker = format!("w:fldCharType=\"{kind}\"");
        assert_eq!(xml.matches(&marker).count(), n, "{kind} markers in {xml}");
    }
}

fn select(ed: &mut Editor, from: Caret, to: Caret) {
    ed.set_caret(from);
    ed.extend_selection(true);
    ed.set_caret(to);
}

#[test]
fn backspace_after_a_spanning_field_selects_it_then_deletes_it_1240() {
    for end_alone in [true, false] {
        let (mut ed, e) = toc("", end_alone);
        let before = ed.doc.clone();
        ed.set_caret(e.clone());
        ed.backspace();
        let range = ed.selection_range().expect("the field is selected");
        assert_eq!(range, (b_of(""), e.clone()), "end_alone {end_alone}");
        assert_eq!(ed.anchor, Some(b_of("")), "the caret keeps its side");
        assert_eq!(ed.doc, before, "selecting changes nothing");
        assert!(!ed.undo(), "selecting is not an undo step");

        ed.backspace();
        assert_eq!(texts(&ed), ["Before", "", "After"]);
        assert_eq!(ed.caret, Caret::at(vec![1], 0));
        assert_no_field(&ed);
        assert!(ed.undo());
        assert_eq!(ed.doc, before, "one undo brings the whole field back");
    }
}

#[test]
fn delete_before_a_spanning_field_selects_it_1240() {
    for end_alone in [true, false] {
        let (mut ed, e) = toc("See ", end_alone);
        let before = ed.doc.clone();
        ed.set_caret(b_of("See "));
        ed.delete_forward();
        assert_eq!(ed.anchor, Some(e.clone()), "the field is selected");
        assert_eq!(ed.caret, b_of("See "), "the caret keeps its side");
        assert_eq!(ed.doc, before);
        assert!(!ed.undo());

        ed.delete_forward();
        assert_eq!(texts(&ed), ["Before", "See ", "After"]);
        assert_no_field(&ed);
    }
}

#[test]
fn deleting_a_spanning_fields_whole_result_leaves_no_shell_1240() {
    type Delete = fn(&mut Editor);
    let deletes: [(&str, Delete); 4] = [
        ("Backspace", |ed| ed.backspace()),
        ("Delete", |ed| ed.delete_forward()),
        ("cut", |ed| {
            ed.delete_selection_keeping_objects();
        }),
        ("typing", |ed| ed.insert_char('Z')),
    ];
    for (name, delete) in deletes {
        for end_alone in [true, false] {
            let (mut ed, e) = toc("See ", end_alone);
            let before = ed.doc.clone();
            select(&mut ed, b_of("See "), e);
            delete(&mut ed);
            let typed = if name == "typing" { "Z" } else { "" };
            let kept = format!("See {typed}");
            assert_eq!(texts(&ed), ["Before", kept.as_str(), "After"], "{name}");
            assert_no_field(&ed);
            assert!(ed.undo());
            assert_eq!(ed.doc, before, "{name}: undo restores it");
        }
    }
}

#[test]
fn a_delete_across_one_edge_takes_the_whole_field_1240() {
    // From before the field into its result.
    let (mut ed, _) = toc("", true);
    select(&mut ed, Caret::at(vec![0], 3), Caret::at(vec![1], 3));
    ed.backspace();
    assert_eq!(texts(&ed), ["Bef", "After"]);
    assert_no_field(&ed);

    // From inside its result to after it.
    let (mut ed, _) = toc("", true);
    select(&mut ed, Caret::at(vec![2], 2), Caret::at(vec![4], 2));
    ed.delete_forward();
    assert_eq!(texts(&ed), ["Before", "ter"]);
    assert_no_field(&ed);

    // Within one paragraph, across its begin: the delete takes the
    // paragraphs of its result too.
    for end_alone in [true, false] {
        let (mut ed, _) = toc("See ", end_alone);
        let before = ed.doc.clone();
        select(&mut ed, Caret::at(vec![1], 2), Caret::at(vec![1], 6));
        ed.backspace();
        assert_eq!(texts(&ed), ["Before", "Se", "After"]);
        assert_no_field(&ed);
        assert!(ed.undo());
        assert_eq!(ed.doc, before);
    }

    // Typing over such a selection: the typed text stays, no marker does.
    let (mut ed, _) = toc("See ", false);
    select(&mut ed, Caret::at(vec![2], 2), Caret::at(vec![3], 1));
    ed.insert_char('Z');
    assert_eq!(texts(&ed), ["Before", "See Zfter"]);
    assert_no_field(&ed);
}

#[test]
fn editing_inside_a_spanning_result_keeps_its_markers_1240() {
    // A guard: inside, the result is text, edited as on main.
    let (mut ed, _) = toc("", true);
    ed.set_caret(Caret::at(vec![1], 3));
    ed.insert_char('x');
    ed.backspace();
    ed.backspace();
    ed.set_caret(Caret::at(vec![2], 2));
    ed.delete_forward();
    ed.insert_newline();
    ed.backspace();
    ed.set_caret(Caret::at(vec![2], 0));
    ed.backspace();
    assert_eq!(texts(&ed), ["Before", "Inro\t1Boy\t2", "", "After"]);
    assert_markers(&ed, 1);
    // A selection strictly inside the result.
    select(&mut ed, Caret::at(vec![1], 1), Caret::at(vec![1], 5));
    ed.backspace();
    assert_eq!(texts(&ed), ["Before", "I1Boy\t2", "", "After"]);
    assert_markers(&ed, 1);
    assert!(!ed.has_selection());
}

/// The inlines of the paragraph at `path`: "begin", "separate", "end",
/// "instr", "run <text>", "tab", "break", "link" or "other".
fn kinds(ed: &Editor, path: &[usize]) -> Vec<String> {
    resolve_para(&ed.doc.body, path)
        .unwrap()
        .content
        .iter()
        .map(|i| match i {
            Inline::Raw(r) if r.contains("\"begin\"") => "begin".to_string(),
            Inline::Raw(r) if r.contains("\"end\"") => "end".to_string(),
            Inline::Raw(r) if r.contains("\"separate\"") => "separate".to_string(),
            Inline::Raw(r) if r.contains("instrText") => "instr".to_string(),
            Inline::Run(r) => format!("run {}", r.text),
            Inline::Tab(_) => "tab".to_string(),
            Inline::Break(..) => "break".to_string(),
            Inline::Hyperlink(_) => "link".to_string(),
            _ => "other".to_string(),
        })
        .collect()
}

type Insert = fn(&mut Editor);

/// Typing, Tab, Shift+Enter and a paste, each with what it inserts.
fn inserts() -> [(&'static str, Insert); 4] {
    [
        ("run x", |ed| ed.insert_char('x')),
        ("tab", |ed| ed.insert_tab()),
        ("break", |ed| ed.insert_line_break()),
        ("run x", |ed| {
            ed.paste(&Clip {
                paras: vec![vec![Inline::Run(Run {
                    text: "x".into(),
                    props: RunProps::default(),
                })]],
            })
        }),
    ]
}

#[test]
fn typing_at_a_spanning_fields_edges_lands_outside_it_1240() {
    for (kind, insert) in inserts() {
        // At b, first in its paragraph.
        let (mut ed, _) = toc("", true);
        ed.set_caret(b_of(""));
        insert(&mut ed);
        assert_eq!(kinds(&ed, &[1])[..2], [kind, "begin"], "{kind} at b");
        // At b, after text.
        let (mut ed, _) = toc("See ", true);
        ed.set_caret(b_of("See "));
        insert(&mut ed);
        let k = kinds(&ed, &[1]);
        let begin = k.iter().position(|k| k == "begin").unwrap();
        assert!(begin >= 2 || k[0] == "run See x", "{kind} at b: {k:?}");
        assert_markers(&ed, 1);
        // At e, alone in its paragraph.
        let (mut ed, e) = toc("", true);
        ed.set_caret(e);
        insert(&mut ed);
        assert_eq!(kinds(&ed, &[3]), ["end", kind], "{kind} at e");
        // At e, closing the last entry.
        let (mut ed, e) = toc("", false);
        ed.set_caret(e);
        insert(&mut ed);
        // (The loader splits a link around its tab.)
        assert_eq!(
            kinds(&ed, &[2]),
            ["link", "tab", "link", "end", kind],
            "{kind} at e"
        );
        assert_markers(&ed, 1);
    }
}

/// A one-paragraph field #642 leaves loose: its result holds a tab.
fn tab_field(instr_text: &str, result: &str) -> String {
    format!(
        "{BEGIN}{}{SEP}<w:r><w:t>{result}</w:t></w:r><w:r><w:tab/></w:r>{END}",
        instr(instr_text)
    )
}

#[test]
fn typing_where_one_loose_field_ends_and_the_next_begins_lands_between_1240() {
    let two = format!("{}{}", tab_field("REF a", "a"), tab_field("REF b", "b"));
    for (kind, insert) in inserts() {
        let mut ed = doc(std::slice::from_ref(&two));
        ed.set_caret(Caret::at(vec![0], 2));
        insert(&mut ed);
        let k = kinds(&ed, &[0]);
        let first_end = k.iter().position(|k| k == "end").unwrap();
        assert_eq!(k[first_end + 1], kind, "{k:?}");
        assert_eq!(k[first_end + 2], "begin", "{k:?}");
    }
}

#[test]
fn a_loose_one_paragraph_field_is_a_unit_too_1240() {
    let p = format!(
        "<w:r><w:t>Body</w:t></w:r>{}<w:r><w:t>z</w:t></w:r>",
        tab_field("REF x", "ab")
    );
    let mut ed = doc(std::slice::from_ref(&p));
    assert!(saved(&ed).contains("fldChar"), "the field is loose");
    assert_eq!(texts(&ed), ["Bodyab\tz"]);
    let before = ed.doc.clone();
    ed.set_caret(Caret::at(vec![0], 7));
    ed.backspace();
    let (lo, hi) = ed.selection_range().expect("the field is selected");
    assert_eq!((lo.offset, hi.offset), (4, 7));
    ed.backspace();
    assert_eq!(texts(&ed), ["Bodyz"]);
    assert_no_field(&ed);
    assert!(ed.undo());
    assert_eq!(ed.doc, before);

    ed.set_caret(Caret::at(vec![0], 4));
    ed.delete_forward();
    let (lo, hi) = ed.selection_range().expect("the field is selected");
    assert_eq!((lo.offset, hi.offset), (4, 7));
    ed.delete_forward();
    assert_eq!(texts(&ed), ["Bodyz"]);
    assert_no_field(&ed);
}

#[test]
fn an_empty_result_loose_field_is_not_a_unit_1240() {
    // An index entry #642 leaves loose (a bookmark in it): nothing to
    // select, so Backspace and Delete go on past it as on main.
    let xe = format!(
        "{BEGIN}{}<w:bookmarkStart w:id=\"1\" w:name=\"m\"/><w:bookmarkEnd w:id=\"1\"/>{END}",
        instr("XE \"term\"")
    );
    let p = format!("<w:r><w:t>term</w:t></w:r>{xe}<w:r><w:t>s</w:t></w:r>");
    let mut ed = doc(std::slice::from_ref(&p));
    assert!(saved(&ed).contains("fldChar"), "the field is loose");
    ed.set_caret(Caret::at(vec![0], 4));
    ed.backspace();
    assert!(!ed.has_selection());
    assert_eq!(texts(&ed), ["ters"]);
    ed.delete_forward();
    assert!(!ed.has_selection());
    assert_eq!(texts(&ed), ["ter"]);
    assert!(saved(&ed).contains("XE"), "the entry stays");

    // Deleting the word just before it keeps it.
    let mut ed = doc(std::slice::from_ref(&p));
    select(&mut ed, Caret::at(vec![0], 0), Caret::at(vec![0], 4));
    ed.backspace();
    assert_eq!(texts(&ed), ["s"]);
    assert!(saved(&ed).contains("XE"), "the entry stays");

    // A selection strictly around it takes it.
    let mut ed = doc(std::slice::from_ref(&p));
    select(&mut ed, Caret::at(vec![0], 3), Caret::at(vec![0], 5));
    ed.backspace();
    assert_eq!(texts(&ed), ["ter"]);
    assert_no_field(&ed);
}

#[test]
fn a_field_paired_through_a_revision_stays_as_today_1240() {
    // A guard against mis-pairing: the inner field's begin is in a tracked
    // insertion, so its paragraph-child end must not close the outer field.
    let inner = format!(
        "<w:ins w:id=\"9\" w:author=\"A\">{BEGIN}</w:ins>{}{SEP}\
         <w:r><w:t>in</w:t></w:r><w:r><w:tab/></w:r>{END}",
        instr("REF y")
    );
    let p = format!(
        "{BEGIN}{}{SEP}<w:r><w:t>a</w:t></w:r>{inner}<w:r><w:t>b</w:t></w:r>{END}",
        instr("REF x")
    );
    let ed = doc(std::slice::from_ref(&p));
    let content = &resolve_para(&ed.doc.body, &[0]).unwrap().content;
    assert!(
        content.iter().any(|i| matches!(i, Inline::Revision { .. })),
        "the inner begin is in a tracked change"
    );
    assert_eq!(texts(&ed), ["ain\tb"]);
    for at in [4, 5] {
        let mut ed = doc(std::slice::from_ref(&p));
        ed.set_caret(Caret::at(vec![0], at));
        ed.backspace();
        assert!(!ed.has_selection(), "no unit ends at {at}");
    }
    let mut ed = doc(std::slice::from_ref(&p));
    ed.set_caret(Caret::at(vec![0], 0));
    ed.delete_forward();
    assert!(!ed.has_selection());
    assert_eq!(texts(&ed), ["in\tb"]);
}

#[test]
fn a_run_shared_by_two_fields_stays_as_today_1240() {
    // A guard: the first field's end and the second's begin share a run.
    let p = format!(
        "{BEGIN}{}{SEP}<w:r><w:t>a</w:t></w:r><w:r><w:tab/></w:r>\
         <w:r><w:fldChar w:fldCharType=\"end\"/><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         {}{SEP}<w:r><w:t>b</w:t></w:r><w:r><w:tab/></w:r>{END}",
        instr("REF x"),
        instr("REF y")
    );
    let mut ed = doc(std::slice::from_ref(&p));
    assert_eq!(texts(&ed), ["a\tb\t"]);
    ed.set_caret(Caret::at(vec![0], 2));
    ed.backspace();
    assert!(!ed.has_selection());
    assert_eq!(texts(&ed), ["ab\t"]);
    // Nor is the second field a unit: its begin run is shared.
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    assert!(!ed.has_selection());
    assert_eq!(texts(&ed), ["ab"]);
}

#[test]
fn tracked_delete_of_a_spanning_field_keeps_its_markers_1240() {
    let track = || TrackAuthor {
        author: "Ann".into(),
        clock: || "2026-10-14T00:00:00Z".into(),
    };
    // Entries as plain runs: a tracked delete across paragraphs drops a
    // link's text whether or not a field is around it, apart from #1240.
    let plain_toc = || {
        doc(&[
            "<w:r><w:t>Before</w:t></w:r>".to_string(),
            format!("{BEGIN}{TOC_INSTR}{SEP}<w:r><w:t>Intro</w:t></w:r>"),
            "<w:r><w:t>Body</w:t></w:r>".to_string(),
            END.to_string(),
            "<w:r><w:t>After</w:t></w:r>".to_string(),
        ])
    };
    let e = Caret::at(vec![3], 0);
    let mut ed = plain_toc();
    ed.set_track_changes(Some(track()));
    let before = ed.doc.clone();
    ed.set_caret(e.clone());
    ed.backspace();
    assert_eq!(ed.selection_range(), Some((b_of(""), e)));
    assert_eq!(ed.doc, before);
    ed.backspace();
    assert_markers(&ed, 1);
    let xml = saved(&ed);
    for text in ["Intro", "Body"] {
        assert!(xml.contains(&format!(">{text}</w:delText>")), "{xml}");
    }

    // Across one edge, the tracked delete widens too.
    let mut ed = plain_toc();
    ed.set_track_changes(Some(track()));
    select(&mut ed, Caret::at(vec![0], 3), Caret::at(vec![1], 2));
    ed.backspace();
    assert_markers(&ed, 1);
    let xml = saved(&ed);
    assert!(xml.contains(">Body</w:delText>"), "{xml}");
}

#[test]
fn replacing_the_end_of_a_loose_result_stays_inside_it_1240() {
    // A guard for Replace: its edits stay where the matched text was, even
    // when the match ends the result.
    let p = format!("<w:r><w:t>Body</w:t></w:r>{}", tab_field("REF x", "ab"));
    let ed = doc(std::slice::from_ref(&p));
    let mut content = resolve_para(&ed.doc.body, &[0]).unwrap().content.clone();
    replace_range_in_content(&mut content, 6, 7, "X");
    assert_eq!(editor_text(&content), "BodyabX");
    let end = content
        .iter()
        .position(|i| matches!(i, Inline::Raw(r) if r.contains("\"end\"")))
        .unwrap();
    assert_eq!(end, content.len() - 1, "X is in the result: {content:?}");
}

#[test]
fn a_cut_across_one_edge_copies_the_whole_field_it_deletes_1240() {
    // r1-M1: the cut copies what its delete takes, in either direction.
    for forward in [true, false] {
        let (mut ed, _) = toc("", true);
        let (lo, hi) = (Caret::at(vec![0], 3), Caret::at(vec![1], 2));
        if forward {
            select(&mut ed, lo, hi);
        } else {
            select(&mut ed, hi, lo);
        }
        let clip = ed.cut().expect("a clip");
        let copied: Vec<String> = clip.paras.iter().map(|p| editor_text(p)).collect();
        assert_eq!(copied, ["ore", "Intro\t1", "Body\t2"], "forward {forward}");
        assert_eq!(texts(&ed), ["Bef", "After"]);
        assert_no_field(&ed);
        // Pasting it back brings the text back (the field itself is not in
        // a clip: zero-width markers never are).
        ed.paste(&clip);
        assert_eq!(texts(&ed), ["Before", "Intro\t1", "Body\t2", "After"]);
    }
}
