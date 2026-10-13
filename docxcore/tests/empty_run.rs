//! A `w:r` that holds no modeled content (only a `w:rPr`, say) is kept on
//! save (#1217): it loads as a zero-width Raw, invisible to the editor, and
//! the regenerating save writes it back where it was.

use docxcore::editor::{Caret, Editor};
use docxcore::export_context::ExportContext;
use docxcore::export_text::to_text;
use docxcore::load::{Relationships, parse_document_xml, parse_rels_xml};
use docxcore::markdown::to_markdown;
use docxcore::model::{Block, Document, Inline};
use docxcore::serialize::document_to_xml;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const EMPTY: &str = "<w:r w:rsidR=\"00A1\"><w:rPr><w:noProof/></w:rPr></w:r>";

const T: &str = "<w:t xml:space=\"preserve\">";

fn rels() -> Relationships {
    parse_rels_xml(
        "<Relationships><Relationship Id=\"rId5\" Target=\"https://contoso.test/\" \
         TargetMode=\"External\"/></Relationships>",
    )
}

fn load(body: &str) -> Document {
    parse_document_xml(
        &format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"urn:rels\"><w:body>{body}</w:body>\
             </w:document>"
        ),
        &rels(),
    )
}

/// The first saved `w:p` of a document.
fn saved_para(doc: &Document) -> String {
    let xml = document_to_xml(doc);
    let start = xml.find("<w:p>").or_else(|| xml.find("<w:p ")).unwrap();
    let end = xml[start..].find("</w:p>").unwrap() + start + "</w:p>".len();
    xml[start..end].to_string()
}

fn content(doc: &Document) -> &[Inline] {
    match &doc.body[0] {
        Block::Paragraph(p) => &p.content,
        other => panic!("expected a paragraph, got {other:?}"),
    }
}

/// The document's text in editor offsets.
fn etext(doc: &Document) -> String {
    let mut ed = Editor::new(doc.clone());
    ed.select_all();
    ed.selection_text()
}

fn at(offset: usize) -> Caret {
    Caret::top(0, offset)
}

#[test]
fn empty_run_survives_regenerating_save() {
    let doc = load(&format!("<w:p>{EMPTY}<w:r><w:t>x</w:t></w:r></w:p>"));
    let p = saved_para(&doc);
    assert!(p.contains(&format!("{EMPTY}<w:r>{T}x</w:t></w:r>")), "{p}");

    // Self-closing, and with whitespace between its children.
    for run in [
        "<w:r/>",
        "<w:r w:rsidR=\"1\"/>",
        "<w:r>\n  <w:rPr>\n    <w:noProof/>\n  </w:rPr>\n</w:r>",
        "<w:r><w:lastRenderedPageBreak/></w:r>",
    ] {
        let doc = load(&format!(
            "<w:p><w:r><w:t>a</w:t></w:r>{run}<w:r><w:t>b</w:t></w:r></w:p>"
        ));
        let p = saved_para(&doc);
        assert!(
            p.contains(&format!("<w:r>{T}a</w:t></w:r>{run}<w:r>{T}b</w:t></w:r>")),
            "{run}: {p}"
        );
    }
}

#[test]
fn empty_run_survives_save_inside_containers() {
    for (open, close) in [
        ("<w:hyperlink r:id=\"rId5\">", "</w:hyperlink>"),
        ("<w:hyperlink w:anchor=\"bm\">", "</w:hyperlink>"),
        (
            "<w:smartTag w:uri=\"urn:x\" w:element=\"place\">",
            "</w:smartTag>",
        ),
        (
            "<w:sdt><w:sdtPr><w:tag w:val=\"t\"/></w:sdtPr><w:sdtContent>",
            "</w:sdtContent></w:sdt>",
        ),
        ("<w:ins w:id=\"1\" w:author=\"A\">", "</w:ins>"),
    ] {
        let doc = load(&format!(
            "<w:p>{open}{EMPTY}<w:r><w:t>x</w:t></w:r>{close}</w:p>"
        ));
        let p = saved_para(&doc);
        let at = p.find(EMPTY).unwrap_or_else(|| panic!("{open}: {p}"));
        assert!(p[at..].contains(close), "{open}: {p}");
        assert!(
            p[..at].contains(open.split(' ').next().unwrap()),
            "{open}: {p}"
        );
        let without = load(&format!("<w:p>{open}<w:r><w:t>x</w:t></w:r>{close}</w:p>"));
        assert_eq!(etext(&doc), etext(&without), "{open}");
    }
}

#[test]
fn empty_run_in_hyperlink_survives_an_edit_of_the_link() {
    for open in [
        "<w:hyperlink r:id=\"rId5\">",
        "<w:hyperlink w:anchor=\"bm\">",
    ] {
        let doc = load(&format!(
            "<w:p><w:r><w:t xml:space=\"preserve\">Visit </w:t></w:r>{open}{EMPTY}\
             <w:r><w:t>Contoso</w:t></w:r></w:hyperlink></w:p>"
        ));
        let mut ed = Editor::new(doc);
        ed.set_caret(at(9));
        ed.insert_char('!');
        assert_eq!(etext(&ed.doc), "Visit Con!toso", "{open}");
        let p = saved_para(&ed.doc);
        assert!(
            p.contains(&format!(
                "{open}{EMPTY}<w:r>{T}Con!toso</w:t></w:r></w:hyperlink>"
            )),
            "{open}: {p}"
        );
        if open.contains("rId5") {
            assert!(
                content(&ed.doc)
                    .iter()
                    .any(|i| matches!(i, Inline::Hyperlink(h)
                    if h.target.as_deref() == Some("https://contoso.test/"))),
                "{:?}",
                content(&ed.doc)
            );
        }
    }
}

#[test]
fn empty_run_adds_no_editor_text() {
    let with = load(&format!(
        "<w:p><w:r><w:t>ab</w:t></w:r>{EMPTY}<w:r><w:t>cd</w:t></w:r></w:p>"
    ));
    let without = load("<w:p><w:r><w:t>ab</w:t></w:r><w:r><w:t>cd</w:t></w:r></w:p>");
    assert_eq!(etext(&with), "abcd");
    assert_eq!(etext(&with), etext(&without));
    assert_eq!(to_markdown(&with), to_markdown(&without));
    let ctx = ExportContext::default();
    assert_eq!(to_text(&with, &ctx), to_text(&without, &ctx));
    assert!(
        content(&with)
            .iter()
            .any(|i| matches!(i, Inline::Raw(r) if r == EMPTY)),
        "{:?}",
        content(&with)
    );

    // The caret moves over it as over nothing: the end is still offset 4.
    let mut ed = Editor::new(with);
    ed.set_caret(at(2));
    ed.insert_char('-');
    assert_eq!(etext(&ed.doc), "ab-cd");
}

#[test]
fn empty_run_survives_edit_in_its_paragraph() {
    let mut ed = Editor::new(load(&format!("<w:p>{EMPTY}<w:r><w:t>x</w:t></w:r></w:p>")));
    ed.set_caret(at(1));
    ed.insert_char('y');
    let p = saved_para(&ed.doc);
    assert!(p.contains(&format!("{EMPTY}<w:r>{T}xy</w:t></w:r>")), "{p}");
}

/// A complex field whose result holds `inner`.
fn field_with(inner: &str) -> String {
    format!(
        "<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
         <w:r><w:instrText xml:space=\"preserve\"> DOCPROPERTY Company </w:instrText></w:r>\
         <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>{inner}\
         <w:r><w:t>Acme</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"
    )
}

#[test]
fn empty_run_inside_complex_field_keeps_field_collapsed() {
    for inner in [EMPTY, "<w:r/>", "<w:r><w:lastRenderedPageBreak/></w:r>"] {
        let field = field_with(inner);
        let doc = load(&format!(
            "<w:p><w:r><w:t>a</w:t></w:r>{field}<w:r><w:t>b</w:t></w:r></w:p>"
        ));
        let c = content(&doc);
        assert_eq!(c.len(), 3, "{inner}: {c:?}");
        assert!(
            matches!(&c[1], Inline::Field { raw, text } if *raw == field && text == "Acme"),
            "{inner}: {c:?}"
        );
        assert!(saved_para(&doc).contains(&field), "{inner}");
    }

    // A run that held only something the loader does not model may show it:
    // it is kept, but the field no longer collapses around it.
    let ruby = "<w:r><w:ruby><w:rubyPr/><w:rt><w:r><w:t>r</w:t></w:r></w:rt>\
                <w:rubyBase><w:r><w:t>b</w:t></w:r></w:rubyBase></w:ruby></w:r>";
    let field = field_with(ruby);
    let doc = load(&format!("<w:p>{field}</w:p>"));
    assert!(
        !content(&doc)
            .iter()
            .any(|i| matches!(i, Inline::Field { .. })),
        "{:?}",
        content(&doc)
    );
    assert!(saved_para(&doc).contains(ruby));
}

#[test]
fn empty_run_in_empty_content_control_still_takes_typing() {
    for inner in ["", EMPTY] {
        let doc = load(&format!(
            "<w:p><w:r><w:t>a</w:t></w:r><w:sdt><w:sdtPr><w:tag w:val=\"t\"/></w:sdtPr>\
             <w:sdtContent>{inner}</w:sdtContent></w:sdt><w:r><w:t>b</w:t></w:r></w:p>"
        ));
        let mut ed = Editor::new(doc);
        ed.set_caret(at(1));
        ed.insert_char('x');
        assert_eq!(etext(&ed.doc), "axb", "{inner:?}");
        let p = saved_para(&ed.doc);
        let open = p.find("<w:sdtContent>").expect("sdtContent") + "<w:sdtContent>".len();
        let close = p.find("</w:sdtContent>").expect("/sdtContent");
        let inside = &p[open..close];
        assert!(inside.contains(&format!("{T}x</w:t>")), "{inner:?}: {p}");
        assert!(inside.contains(inner), "{inner:?}: {p}");
    }
}

#[test]
fn empty_run_declares_a_prefix_bound_below_the_part_root() {
    // `x` is bound on `w:body` or a `w:tc`, neither of which the save keeps.
    let run = "<w:r><w:rPr><x:foo/></w:rPr></w:r>";
    let para = format!("<w:p>{run}<w:r><w:t>a</w:t></w:r></w:p>");
    for xml in [
        format!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body xmlns:x=\"urn:x\">{para}</w:body>\
             </w:document>"
        ),
        format!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl><w:tr><w:tc xmlns:x=\"urn:x\">\
             {para}</w:tc></w:tr></w:tbl></w:body></w:document>"
        ),
    ] {
        let doc = parse_document_xml(&xml, &Relationships::default());
        let saved = document_to_xml(&doc);
        let declared = "<w:r xmlns:x=\"urn:x\"><w:rPr><x:foo/></w:rPr></w:r>";
        assert!(saved.contains(declared), "{saved}");

        // It reloads as the same empty run, which the next save keeps once.
        let again = document_to_xml(&parse_document_xml(&saved, &Relationships::default()));
        assert_eq!(again.matches(declared).count(), 1, "{again}");
        assert_eq!(again.matches("<x:foo/>").count(), 1, "{again}");
    }
}

#[test]
fn a_raw_kept_run_declares_a_prefix_bound_below_the_part_root() {
    // The same gap in a run kept whole for a footnote reference.
    let run = "<w:r><w:rPr><x:foo/></w:rPr><w:footnoteReference w:id=\"1\"/></w:r>";
    let xml = format!(
        "<w:document xmlns:w=\"{W_NS}\"><w:body xmlns:x=\"urn:x\"><w:p>{run}</w:p></w:body>\
         </w:document>"
    );
    let saved = document_to_xml(&parse_document_xml(&xml, &Relationships::default()));
    assert!(
        saved.contains(
            "<w:r xmlns:x=\"urn:x\"><w:rPr><x:foo/></w:rPr><w:footnoteReference w:id=\"1\"/></w:r>"
        ),
        "{saved}"
    );
}
