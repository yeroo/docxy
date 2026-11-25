//! HTML import page margins (#1928): an imported page is a Letter page with
//! Outlook-like 15 px (225 twip) margins, and its package, a save and a load
//! back all keep them. A page's own `@page` gives its page instead (#1944),
//! but for a classic-Outlook mail.

use docxcore::import::import_html_with;
use docxcore::model::PageGeom;
use docxcore::package::{load_package, save_package};

fn assert_outlook_page(g: PageGeom, what: &str) {
    assert_eq!(
        (g.w, g.h, g.ml, g.mr, g.mt, g.mb, g.cols),
        (12_240, 15_840, 225, 225, 225, 225, 1),
        "{what}"
    );
}

#[test]
fn html_page_margins_survive_package_and_save() {
    let page = b"<html><body><table width=600 align=center><tr><td>A mail body</td></tr>\
                 </table><p>after</p></body></html>";
    let mut none = |_: &str, _: usize| None;
    let pkg = import_html_with(page, &mut none).unwrap().package();
    assert_outlook_page(pkg.page_geom(), "the package");
    assert!(pkg.sect_pr().contains("<w:pgMar"), "{}", pkg.sect_pr());

    let saved = save_package(&pkg);
    let back = load_package(&saved).unwrap();
    assert_outlook_page(back.page_geom(), "loaded back");
    let xml = String::from_utf8(back.part("word/document.xml").unwrap().to_vec()).unwrap();
    // One final section, after the body's content.
    assert_eq!(xml.matches("<w:sectPr").count(), 1, "{xml}");
    let at = |s: &str| xml.find(s).unwrap_or_else(|| panic!("no {s} in {xml}"));
    assert!(at("after") < at("<w:sectPr"), "{xml}");
    assert!(
        xml.contains(
            r#"<w:pgMar w:top="225" w:right="225" w:bottom="225" w:left="225" w:header="225" w:footer="225" w:gutter="0"/>"#
        ),
        "{xml}"
    );
    assert!(
        xml.contains(r#"<w:pgSz w:w="12240" w:h="15840"/>"#),
        "{xml}"
    );
    assert_eq!(back.document.content_block_count(), 2);
}

/// #1944: a classic-Outlook mail carries Word's default `@page` (1"
/// margins) that its sender never chose: it keeps the Outlook-like page.
#[test]
fn an_outlook_mail_keeps_the_outlook_page() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/html/outlook-reply.html"
    );
    let page = std::fs::read(path).unwrap();
    let text = String::from_utf8_lossy(&page);
    assert!(text.contains("(filtered medium)") && text.contains("@page WordSection1"));
    let mut none = |_: &str, _: usize| None;
    let pkg = import_html_with(&page, &mut none).unwrap().package();
    assert_outlook_page(pkg.page_geom(), "an Outlook mail");
}

/// #1944: a Word page's own `@page` (1" top and bottom, 1.25" sides, a
/// header 0.5" from the edge) survives the package, a save and a load
/// back.
#[test]
fn a_word_page_keeps_its_page_through_package_and_save() {
    let page = b"<html><head><meta name=Generator content=\"Microsoft Word 15\"><style>\
                 @page WordSection1 {size:8.5in 11.0in; margin:1.0in 1.25in 1.0in 1.25in;\
                 mso-header-margin:.5in; mso-footer-margin:.5in;}\
                 div.WordSection1 {page:WordSection1;}</style></head>\
                 <body><div class=WordSection1><p>A Word page</p></div></body></html>";
    let mut none = |_: &str, _: usize| None;
    let pkg = import_html_with(page, &mut none).unwrap().package();
    let word = |g: PageGeom| (g.w, g.h, g.ml, g.mr, g.mt, g.mb);
    assert_eq!(
        word(pkg.page_geom()),
        (12_240, 15_840, 1800, 1800, 1440, 1440)
    );
    let back = load_package(&save_package(&pkg)).unwrap();
    assert_eq!(
        word(back.page_geom()),
        (12_240, 15_840, 1800, 1800, 1440, 1440)
    );
    let xml = String::from_utf8(back.part("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        xml.contains(
            r#"<w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" w:header="720" w:footer="720" w:gutter="0"/>"#
        ),
        "{xml}"
    );
}
