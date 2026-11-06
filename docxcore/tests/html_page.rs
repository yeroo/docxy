//! HTML import page margins (#1928): an imported page is a Letter page with
//! Outlook-like 15 px (225 twip) margins, and its package, a save and a load
//! back all keep them.

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
