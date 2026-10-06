//! Run content beyond `w:t` survives a save from the model (#1101): the
//! non-breaking and soft hyphens load as U+2011 / U+00AD text and write back
//! as `w:noBreakHyphen` / `w:softHyphen`, and every other run-content element
//! either round-trips or is rewritten to its stated equivalent.

use docxcore::editor::{Caret, Editor};
use docxcore::load::{Relationships, parse_document_xml};
use docxcore::model::{Block, Document, Inline, Run};
use docxcore::render::{RenderOptions, render};
use docxcore::serialize::document_to_xml;

fn load(body: &str) -> Document {
    parse_document_xml(
        &format!("<w:document><w:body>{body}</w:body></w:document>"),
        &Relationships::default(),
    )
}

fn para_text(doc: &Document) -> String {
    match &doc.body[0] {
        Block::Paragraph(p) => p.content.iter().map(Inline::text).collect(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

fn runs(doc: &Document) -> Vec<&Run> {
    match &doc.body[0] {
        Block::Paragraph(p) => p
            .content
            .iter()
            .filter_map(|i| match i {
                Inline::Run(r) => Some(r),
                _ => None,
            })
            .collect(),
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

/// The saved `w:p` of a one-paragraph document.
fn saved_para(doc: &Document) -> String {
    let xml = document_to_xml(doc);
    let start = xml.find("<w:p>").or_else(|| xml.find("<w:p ")).unwrap();
    let end = xml.find("</w:p>").unwrap() + "</w:p>".len();
    xml[start..end].to_string()
}

#[test]
fn a_non_breaking_hyphen_loads_as_u2011_and_saves_as_the_element() {
    let doc = load("<w:p><w:r><w:t>e</w:t><w:noBreakHyphen/><w:t>commerce</w:t></w:r></w:p>");
    assert_eq!(para_text(&doc), "e\u{2011}commerce");
    // One `w:r` is one Run, so the save writes one `w:r` again.
    assert_eq!(runs(&doc).len(), 1);
    let p = saved_para(&doc);
    assert!(
        p.contains(
            "<w:r><w:t xml:space=\"preserve\">e</w:t><w:noBreakHyphen/>\
             <w:t xml:space=\"preserve\">commerce</w:t></w:r>"
        ),
        "{p}"
    );
    assert!(!p.contains('\u{2011}'), "{p}");
    assert_eq!(
        parse_document_xml(&document_to_xml(&doc), &Relationships::default()),
        doc
    );
}

#[test]
fn a_soft_hyphen_loads_as_u00ad_and_saves_as_the_element() {
    let doc = load("<w:p><w:r><w:t>insufficien</w:t><w:softHyphen/><w:t>tly</w:t></w:r></w:p>");
    assert_eq!(para_text(&doc), "insufficien\u{ad}tly");
    let p = saved_para(&doc);
    assert!(
        p.contains(
            "<w:t xml:space=\"preserve\">insufficien</w:t><w:softHyphen/>\
             <w:t xml:space=\"preserve\">tly</w:t>"
        ),
        "{p}"
    );
    assert!(!p.contains('\u{ad}'), "{p}");
    assert_eq!(
        parse_document_xml(&document_to_xml(&doc), &Relationships::default()),
        doc
    );
}

#[test]
fn hyphens_at_a_runs_edges_and_between_whitespace_stay_in_one_run() {
    let doc = load(
        "<w:p><w:r><w:rPr><w:b/></w:rPr><w:noBreakHyphen/>\n  <w:t>a</w:t>\n  \
         <w:softHyphen/><w:noBreakHyphen/></w:r></w:p>",
    );
    assert_eq!(para_text(&doc), "\u{2011}a\u{ad}\u{2011}");
    assert_eq!(runs(&doc).len(), 1);
    assert!(runs(&doc)[0].props.bold);
    let p = saved_para(&doc);
    assert!(
        p.contains(
            "<w:noBreakHyphen/><w:t xml:space=\"preserve\">a</w:t>\
             <w:softHyphen/><w:noBreakHyphen/></w:r>"
        ),
        "{p}"
    );
    assert_eq!(
        parse_document_xml(&document_to_xml(&doc), &Relationships::default()),
        doc
    );
}

#[test]
fn a_run_holding_only_a_soft_hyphen_is_kept() {
    // complex0.docx, just before bookmark A173.
    let doc = load(
        "<w:p><w:r><w:softHyphen/></w:r><w:bookmarkStart w:id=\"127\" w:name=\"A173\"/>\
         <w:bookmarkEnd w:id=\"127\"/></w:p>",
    );
    assert_eq!(para_text(&doc), "\u{ad}");
    let p = saved_para(&doc);
    assert!(
        p.contains("<w:r><w:softHyphen/></w:r>"),
        "no empty w:t: {p}"
    );
}

#[test]
fn a_deleted_runs_hyphen_is_the_same_element() {
    let doc = load(
        "<w:p><w:del w:id=\"1\" w:author=\"A\"><w:r><w:delText>e</w:delText>\
         <w:noBreakHyphen/><w:delText>mail</w:delText></w:r></w:del></w:p>",
    );
    assert_eq!(para_text(&doc), "e\u{2011}mail");
    // An unedited revision is kept verbatim; an edited one is written from
    // its runs.
    let mut doc = doc;
    let Block::Paragraph(para) = &mut doc.body[0] else {
        unreachable!()
    };
    let Inline::Revision {
        content_changed, ..
    } = &mut para.content[0]
    else {
        panic!("expected a revision: {:?}", para.content);
    };
    *content_changed = true;
    let p = saved_para(&doc);
    assert!(
        p.contains(
            "<w:delText xml:space=\"preserve\">e</w:delText><w:noBreakHyphen/>\
             <w:delText xml:space=\"preserve\">mail</w:delText>"
        ),
        "{p}"
    );
}

#[test]
fn a_hyphen_inside_a_hyperlink_round_trips() {
    let doc = load(
        "<w:p><w:hyperlink w:anchor=\"x\"><w:r><w:t>ITU</w:t><w:noBreakHyphen/>\
         <w:t>T</w:t></w:r></w:hyperlink></w:p>",
    );
    assert_eq!(para_text(&doc), "ITU\u{2011}T");
    let p = saved_para(&doc);
    assert!(p.contains("ITU</w:t><w:noBreakHyphen/><w:t"), "{p}");
}

#[test]
fn literal_hyphen_characters_in_text_stay_text() {
    let doc = load("<w:p><w:r><w:t>e\u{2011}mail co\u{ad}op</w:t></w:r></w:p>");
    assert_eq!(para_text(&doc), "e\u{2011}mail co\u{ad}op");
    let p = saved_para(&doc);
    assert!(
        p.contains("<w:t xml:space=\"preserve\">e\u{2011}mail co\u{ad}op</w:t>"),
        "{p}"
    );
    assert!(!p.contains("Hyphen/>"), "{p}");
}

const ECOMMERCE: &str = "<w:p><w:r><w:t>e</w:t><w:noBreakHyphen/><w:t>commerce</w:t></w:r></w:p>";

fn hyphen_elements(doc: &Document) -> usize {
    document_to_xml(doc).matches("<w:noBreakHyphen/>").count()
}

#[test]
fn typing_next_to_a_hyphen_keeps_it() {
    for offset in [1, 2] {
        let mut ed = Editor::new(load(ECOMMERCE));
        ed.set_caret(Caret::top(0, offset));
        ed.insert_char('x');
        let expected = if offset == 1 {
            "ex\u{2011}commerce"
        } else {
            "e\u{2011}xcommerce"
        };
        assert_eq!(para_text(&ed.doc), expected);
        assert_eq!(hyphen_elements(&ed.doc), 1, "typed at {offset}");
        let p = saved_para(&ed.doc);
        assert!(!p.contains('\u{2011}'), "typed at {offset}: {p}");
        assert!(
            !p.contains(">x<"),
            "a typed letter is text, not an element: {p}"
        );
    }
}

#[test]
fn copy_and_paste_keeps_the_hyphen() {
    let mut ed = Editor::new(load(ECOMMERCE));
    ed.anchor = Some(Caret::top(0, 0));
    ed.caret = Caret::top(0, 4);
    let clip = ed.copy().expect("a selection");
    assert_eq!(clip.to_text(), "e\u{2011}co");
    ed.anchor = None;
    ed.caret = Caret::top(0, 10);
    ed.paste(&clip);
    assert_eq!(para_text(&ed.doc), "e\u{2011}commercee\u{2011}co");
    assert_eq!(hyphen_elements(&ed.doc), 2);
    assert!(!saved_para(&ed.doc).contains('\u{2011}'));
}

#[test]
fn clearing_formatting_keeps_the_hyphen_an_element() {
    let mut ed = Editor::new(load(
        "<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>e</w:t><w:noBreakHyphen/>\
         <w:t>commerce</w:t></w:r></w:p>",
    ));
    ed.select_all();
    ed.clear_run_formatting();
    assert!(!runs(&ed.doc)[0].props.bold);
    assert_eq!(hyphen_elements(&ed.doc), 1);
    assert!(!saved_para(&ed.doc).contains('\u{2011}'));
}

fn rendered(doc: &Document, width: usize) -> Vec<String> {
    let opts = RenderOptions {
        width,
        ..RenderOptions::default()
    };
    render(doc, &opts)
        .iter()
        .map(|l| l.plain().trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[test]
fn a_non_breaking_hyphen_shows_as_a_hyphen_and_never_breaks_a_line() {
    // `abcde e-` fills the 8 columns (the narrowest render); a break after
    // the hyphen would leave it there, but the line breaks at the space.
    let doc = load("<w:p><w:r><w:t>abcde e</w:t><w:noBreakHyphen/><w:t>cd</w:t></w:r></w:p>");
    assert_eq!(rendered(&doc, 8), ["abcde", "e-cd"]);
}

#[test]
fn a_soft_hyphen_takes_no_column() {
    // From the element or as literal text, it is the same character.
    let doc = load("<w:p><w:r><w:t>abcdefg</w:t><w:softHyphen/><w:t>h</w:t></w:r></w:p>");
    assert_eq!(rendered(&doc, 8), ["abcdefgh"]);
    let doc = load("<w:p><w:r><w:t>abcdefg\u{ad}h</w:t></w:r></w:p>");
    assert_eq!(rendered(&doc, 8), ["abcdefgh"]);
}

/// What a save writes for each run-content element the issue lists, from a
/// run `<w:r><w:t>a</w:t>{element}</w:r>`.
#[test]
fn every_listed_run_content_element_survives_a_save() {
    let cases: &[(&str, &str)] = &[
        // Modeled: written back from the model.
        ("<w:noBreakHyphen/>", "<w:noBreakHyphen/>"),
        ("<w:softHyphen/>", "<w:softHyphen/>"),
        ("<w:tab/>", "<w:tab/>"),
        ("<w:br/>", "<w:br/>"),
        ("<w:br w:type=\"page\"/>", "<w:br w:type=\"page\"/>"),
        ("<w:br w:type=\"column\"/>", "<w:br w:type=\"column\"/>"),
        // A carriage return is the same text-wrapping break: it is rewritten
        // to the equivalent `w:br`.
        ("<w:cr/>", "<w:br/>"),
        // Not modeled: the run is kept verbatim.
        (
            "<w:sym w:font=\"Wingdings\" w:char=\"F0E0\"/>",
            "<w:sym w:font=\"Wingdings\" w:char=\"F0E0\"/>",
        ),
        (
            "<w:ptab w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"none\"/>",
            "<w:ptab w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"none\"/>",
        ),
        ("<w:dayShort/>", "<w:dayShort/>"),
        ("<w:dayLong/>", "<w:dayLong/>"),
        ("<w:monthShort/>", "<w:monthShort/>"),
        ("<w:monthLong/>", "<w:monthLong/>"),
        ("<w:yearShort/>", "<w:yearShort/>"),
        ("<w:yearLong/>", "<w:yearLong/>"),
        ("<w:pgNum/>", "<w:pgNum/>"),
        ("<w:separator/>", "<w:separator/>"),
        ("<w:continuationSeparator/>", "<w:continuationSeparator/>"),
        ("<w:annotationRef/>", "<w:annotationRef/>"),
        ("<w:footnoteRef/>", "<w:footnoteRef/>"),
        ("<w:endnoteRef/>", "<w:endnoteRef/>"),
    ];
    for (element, saved) in cases {
        let doc = load(&format!("<w:p><w:r><w:t>a</w:t>{element}</w:r></w:p>"));
        let p = saved_para(&doc);
        assert!(p.contains(saved), "{element} saved as {p}");
        assert!(
            p.contains(">a</w:t>"),
            "{element}: the run's text is kept: {p}"
        );
    }
}

/// `w:lastRenderedPageBreak` is Word's layout cache, which Word regenerates:
/// it stays dropped on purpose.
#[test]
fn a_last_rendered_page_break_is_not_written() {
    let doc = load("<w:p><w:r><w:lastRenderedPageBreak/><w:t>a</w:t></w:r></w:p>");
    assert!(!saved_para(&doc).contains("lastRenderedPageBreak"));
}

/// Rejecting a formatting change restores the old properties, not how the
/// run's hyphens are written (review r1).
#[test]
fn rejecting_a_formatting_change_keeps_the_hyphen_an_element() {
    for (element, ch) in [
        ("<w:noBreakHyphen/>", '\u{2011}'),
        ("<w:softHyphen/>", '\u{ad}'),
    ] {
        let mut doc = load(&format!(
            "<w:p><w:r><w:rPr><w:b/><w:rPrChange w:id=\"1\" w:author=\"A\"><w:rPr/>\
             </w:rPrChange></w:rPr><w:t>e</w:t>{element}<w:t>mail</w:t></w:r></w:p>"
        ));
        doc.initialize_revision_targets();
        doc.reject_all_revisions();
        assert!(!runs(&doc)[0].props.bold, "{element}: rejected");
        assert_eq!(para_text(&doc), format!("e{ch}mail"));
        let p = saved_para(&doc);
        assert!(p.contains(element), "{element}: {p}");
        assert!(!p.contains(ch), "{element}: {p}");
    }
}

/// Literal hyphen characters and hyphen elements in one `w:r` never share a
/// Run, so each saves as it was (review r1).
#[test]
fn literal_and_element_hyphens_in_one_run_each_save_as_they_were() {
    for body in [
        "<w:t>a\u{2011}b</w:t><w:softHyphen/><w:t>c</w:t>",
        "<w:t>a</w:t><w:softHyphen/><w:t>b\u{2011}c</w:t>",
        "<w:t>a</w:t><w:noBreakHyphen/><w:t>\u{ad}</w:t><w:noBreakHyphen/>",
    ] {
        let doc = load(&format!("<w:p><w:r>{body}</w:r></w:p>"));
        let p = saved_para(&doc);
        let elements = body.matches("Hyphen/>").count();
        assert_eq!(p.matches("Hyphen/>").count(), elements, "{body}: {p}");
        let literals = body.matches(['\u{2011}', '\u{ad}']).count();
        assert_eq!(
            p.matches(['\u{2011}', '\u{ad}']).count(),
            literals,
            "{body}: {p}"
        );
        assert_eq!(
            parse_document_xml(&document_to_xml(&doc), &Relationships::default()),
            doc
        );
    }
}

/// A soft hyphen takes no column before a tab: the tab stops where it would
/// without it, left or right aligned (review r1).
#[test]
fn a_soft_hyphen_does_not_move_a_tab() {
    let pair = |ppr: &str, with: &str, without: &str| {
        let doc = |runs: &str| load(&format!("<w:p>{ppr}<w:r>{runs}</w:r></w:p>"));
        (rendered(&doc(with), 40), rendered(&doc(without), 40))
    };
    let (with, without) = pair(
        "",
        "<w:t>abcdefg</w:t><w:softHyphen/><w:tab/><w:t>x</w:t>",
        "<w:t>abcdefg</w:t><w:tab/><w:t>x</w:t>",
    );
    assert_eq!(with, without);
    assert_eq!(with, ["abcdefg x"]);
    // The text after a right tab is measured without it.
    let (with, without) = pair(
        "<w:pPr><w:tabs><w:tab w:val=\"right\" w:pos=\"4000\"/></w:tabs></w:pPr>",
        "<w:t>q</w:t><w:tab/><w:t>a</w:t><w:softHyphen/><w:t>b</w:t>",
        "<w:t>q</w:t><w:tab/><w:t>ab</w:t>",
    );
    assert_eq!(with, without);
}

/// A soft hyphen at the end of a paragraph keeps the end's caret stop, so
/// End reaches past it (review r1).
#[test]
fn a_trailing_soft_hyphen_keeps_the_end_caret_stop() {
    let doc = load("<w:p><w:r><w:t>abc</w:t><w:softHyphen/></w:r></w:p>");
    let (_, maps) = docxcore::render::render_mapped(&doc, &RenderOptions::default());
    let seg = &maps[0].segs[0];
    let last = seg.visual.last().unwrap();
    assert_eq!((last.offset, last.col), (4, 3), "{:?}", seg.visual);
}
