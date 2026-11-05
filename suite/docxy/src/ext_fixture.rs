//! The harness's test extension (#1873): a Mail surface whose body is a
//! header line over an [`ext::DocView`] of a fixture document, so a `.uit`
//! case can drive the embedded document view as an extension would place it.
//! Installed only in a harness instance whose script asks for it
//! (`ext fixture`, [`crate::harness::ext_fixture_requested`]); the public
//! `suite` binary and any other run never make it.
//!
//! It uses only what an extension crate can: the public items of
//! [`crate::ext`] (`tests/ext_doc_view_api.rs` holds the same surface to that
//! from outside the crate), and records every link the view hands it.

use crate::ext::{self, DocView, TabSurface};
use docxcore::model::Document;
use docxcore::package::{Package, new_package};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The target of the fixture document's hyperlink.
pub(crate) const LINK_URL: &str = "https://example.com/docxy";
/// The target of its indented paragraph's link.
pub(crate) const INDENTED_URL: &str = "https://example.com/indented";

/// The fixture's view: what the harness downcasts the Mail tab's body to.
pub(crate) struct FixtureView {
    pub(crate) doc: Entity<DocView>,
    /// Each link the view handed the extension, in order.
    pub(crate) link_calls: Rc<RefCell<Vec<String>>>,
}

impl Render for FixtureView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let calls = self.link_calls.borrow().len();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .when(calls > 0, |d| d.font_weight(FontWeight::BOLD))
                    .child(format!("Fixture extension ({calls} links followed)")),
            )
            .child(div().flex_1().min_h(px(0.)).child(self.doc.clone()))
    }
}

struct FixtureMail {
    root: Entity<FixtureView>,
}

impl TabSurface for FixtureMail {
    fn view(&self) -> AnyView {
        self.root.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.root.read(cx).doc.read(cx).focus_handle()
    }
}

/// The fixture's Mail factory.
pub(crate) fn factory() -> ext::Factory {
    Rc::new(|cx: &mut App| {
        let (doc, pkg) = fixture_document();
        let calls: Rc<RefCell<Vec<String>>> = Rc::default();
        let view = cx.new(|cx| DocView::new(doc, Some(pkg), cx));
        view.update(cx, |v, _| {
            let calls = calls.clone();
            v.on_link(move |url, _window, _cx| calls.borrow_mut().push(url.to_string()));
        });
        let root = cx.new(|_| FixtureView {
            doc: view,
            link_calls: calls,
        });
        Ok(Box::new(FixtureMail { root }) as Box<dyn TabSurface>)
    })
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn para(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

fn run(text: &str) -> String {
    format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>")
}

fn cell(text: &str) -> String {
    format!(
        "<w:tc><w:tcPr><w:tcW w:w=\"2400\" w:type=\"dxa\"/></w:tcPr>{}</w:tc>",
        para(&run(text))
    )
}

/// The fixture document and its package, built here (no binary fixture): a
/// heading, `Alpha Beta Gamma` (Beta bold), a hyperlink `docxy link` to
/// [`LINK_URL`], an indented paragraph that starts with a link
/// (`Indented link`, to [`INDENTED_URL`]), an inline picture
/// (`uiharness/fixtures/logo.png`), a 2x2 table `A1 B1 / A2 B2`, then
/// `Line 1`..`Line 80` so it scrolls.
pub(crate) fn fixture_document() -> (Document, Package) {
    let mut pkg = new_package(Document { body: vec![] });
    let png: &[u8] = include_bytes!("../../../uiharness/fixtures/logo.png");
    let kind = docxcore::image::sniff(png).expect("logo.png is a png");
    let rid = pkg
        .add_part_image("word/document.xml", png, &kind)
        .expect("the picture is added");
    let rels_name = "word/_rels/document.xml.rels";
    let rels = pkg.part_text(rels_name).expect("document rels").replace(
        "</Relationships>",
        &format!(
            "<Relationship Id=\"rIdLink1\" Type=\"{R}/hyperlink\" Target=\"{LINK_URL}\" TargetMode=\"External\"/>\
             <Relationship Id=\"rIdLink2\" Type=\"{R}/hyperlink\" Target=\"{INDENTED_URL}\" TargetMode=\"External\"/></Relationships>"
        ),
    );
    assert!(pkg.set_part(rels_name, rels.into_bytes()));
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:styles xmlns:w=\"{W}\"><w:style w:type=\"paragraph\" w:styleId=\"Heading1\">\
         <w:name w:val=\"heading 1\"/><w:rPr><w:b/><w:sz w:val=\"32\"/></w:rPr></w:style></w:styles>"
    );
    assert!(pkg.set_part("word/styles.xml", styles.into_bytes()));
    let mut body = String::new();
    body += &para(&format!(
        "<w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>{}",
        run("Embedded view")
    ));
    body += &para(&format!(
        "{}<w:r><w:rPr><w:b/></w:rPr><w:t>Beta</w:t></w:r>{}",
        run("Alpha "),
        run(" Gamma")
    ));
    body += &para(&format!(
        "{}<w:hyperlink r:id=\"rIdLink1\">{}</w:hyperlink>{}",
        run("Follow the "),
        run("docxy link"),
        run(" here.")
    ));
    // A row that starts with a link, indented: a press in its indent is
    // beside the row, never on the link (#1873 r1).
    body += &para(&format!(
        "<w:pPr><w:ind w:left=\"1440\"/></w:pPr><w:hyperlink r:id=\"rIdLink2\">{}</w:hyperlink>{}",
        run("Indented link"),
        run(" after it")
    ));
    body += &para(&docxcore::image::inline_picture_run(
        &rid, 609600, 304800, 1, "logo.png",
    ));
    body += &format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"4800\" w:type=\"dxa\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid>\
         <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
        cell("A1"),
        cell("B1"),
        cell("A2"),
        cell("B2")
    );
    for i in 1..=80 {
        body += &para(&run(&format!("Line {i}")));
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}</w:body></w:document>"
    );
    assert!(pkg.set_part("word/document.xml", xml.clone().into_bytes()));
    let doc = docxcore::load::parse_document_xml(&xml, &pkg.part_rels("word/document.xml"));
    (doc, pkg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::editor::{Caret, Editor};
    use docxcore::model::{Block, Inline};

    #[test]
    fn the_fixture_document_has_a_link_a_table_and_a_resolving_picture() {
        let (doc, pkg) = fixture_document();
        // Heading, Alpha..., link, indented link, picture, table, 80 lines.
        assert_eq!(doc.body.len(), 86);
        assert!(matches!(doc.body[5], Block::Table(_)));
        let Block::Paragraph(indented) = &doc.body[3] else {
            panic!("a paragraph");
        };
        assert_eq!(
            crate::doc_view::link_at(indented, 0).as_deref(),
            Some(INDENTED_URL)
        );
        assert_eq!(indented.props.indent, 1440);
        let Block::Paragraph(link) = &doc.body[2] else {
            panic!("a paragraph");
        };
        assert_eq!(
            crate::doc_view::link_at(link, "Follow the ".len()).as_deref(),
            Some(LINK_URL)
        );
        let Block::Paragraph(pic) = &doc.body[4] else {
            panic!("a paragraph");
        };
        let raw = pic
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Raw(r) => Some(r.clone()),
                _ => None,
            })
            .expect("the picture run");
        let rid = docxcore::image::inline_embed(&raw).expect("an inline picture");
        let (ext, _) = crate::pictures::resolve(&pkg, "word/document.xml", &rid).expect("resolves");
        assert_eq!(ext, "png");
        // A table cell's paragraph is reached by its path.
        let a2 = docxcore::editor::resolve_para(&doc.body, &[5, 1, 0, 0]).expect("A2");
        assert_eq!(a2.plain_text(), "A2");
    }

    #[test]
    fn a_selection_of_the_fixture_copies_its_text_and_bold_in_the_rtf() {
        let (doc, pkg) = fixture_document();
        let mut ed = Editor::new(doc);
        ed.anchor = Some(Caret::at(vec![1], 0));
        ed.caret = Caret::at(vec![1], "Alpha Beta Gamma".len());
        let (text, rtf) = crate::doc_view::copy_payload(&ed, Some(&pkg)).expect("a copy");
        assert_eq!(text, "Alpha Beta Gamma");
        let rtf = rtf.expect("RTF beside the text");
        assert!(rtf.contains("\\b"), "{rtf}");
        assert!(rtf.contains("Beta"), "{rtf}");
        // Nothing selected copies nothing.
        ed.anchor = None;
        assert!(crate::doc_view::copy_payload(&ed, Some(&pkg)).is_none());
        // Select All copies everything, the last line included.
        ed.select_all();
        let (all, _) = crate::doc_view::copy_payload(&ed, Some(&pkg)).expect("a copy");
        assert!(all.starts_with("Embedded view"), "{all}");
        assert!(all.trim_end().ends_with("Line 80"), "{all}");
    }
}
