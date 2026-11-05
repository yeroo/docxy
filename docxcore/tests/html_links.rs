//! HTML import links (#1879): a newsletter-like page's `<a href>` links
//! (styled text inside one, a mailto, an anchor link, a `javascript:`
//! button, a relative link, a link in a table cell and one around a
//! `<br>`), then saved in a package and loaded back.

use docxcore::import::{import_html, import_html_with};
use docxcore::model::{Block, Document, Hyperlink, Inline, Paragraph};
use docxcore::package::{load_package, save_package};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/html")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn paragraphs(doc: &Document) -> Vec<&Paragraph> {
    fn walk<'a>(blocks: &'a [Block], out: &mut Vec<&'a Paragraph>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => out.push(p),
                Block::Table(t) => {
                    for cell in t.rows.iter().flat_map(|r| &r.cells) {
                        walk(&cell.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.body, &mut out);
    out
}

fn links(doc: &Document) -> Vec<&Hyperlink> {
    paragraphs(doc)
        .into_iter()
        .flat_map(|p| &p.content)
        .filter_map(|i| match i {
            Inline::Hyperlink(h) => Some(h),
            _ => None,
        })
        .collect()
}

/// Each link as where it goes (a URL, or `#anchor`) and its text.
fn summary(doc: &Document) -> Vec<(String, String)> {
    links(doc)
        .iter()
        .map(|h| {
            let to = match (&h.target, &h.anchor) {
                (Some(t), _) => t.clone(),
                (None, Some(a)) => format!("#{a}"),
                (None, None) => String::new(),
            };
            (to, h.runs.iter().map(|r| r.text.as_str()).collect())
        })
        .collect()
}

fn expected() -> Vec<(String, String)> {
    [
        ("https://example.com/weekly", "digest"),
        ("https://example.com/post?id=7&src=mail", "the post online"),
        ("mailto:editor@example.com", "write to us"),
        ("https://example.com/prefs", "Preferences"),
        ("https://sponsor.example.net/", "Sponsor Inc"),
        ("https://example.com/map", "Main St"),
        ("https://example.com/map", "Springfield"),
        ("#top", "Back to top"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

#[test]
fn a_newsletter_s_links_become_hyperlinks() {
    let d = import_html(&fixture("links.html")).unwrap();
    assert_eq!(summary(&d), expected());
    // The unsafe and the relative ones are text, in the paragraph's runs.
    let buttons = paragraphs(&d)
        .into_iter()
        .find(|p| p.plain_text().contains("Subscribe"))
        .unwrap();
    assert_eq!(
        buttons.plain_text(),
        "Subscribe | Unsubscribe | Preferences"
    );
    let plain: Vec<&str> = buttons
        .content
        .iter()
        .filter_map(|i| match i {
            Inline::Run(r) => Some(r.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(plain.concat(), "Subscribe | Unsubscribe | ");
    // The page's `a` colour is over the user agent's; a styled span inside
    // keeps its weight; `a.button`'s reset gives the body's colour back
    // and no underline.
    let ls = links(&d);
    let post = ls[1];
    assert!(post.runs.iter().all(|r| r.props.underline));
    assert!(
        post.runs
            .iter()
            .all(|r| r.props.color.as_deref() == Some("1A73E8"))
    );
    assert!(post.runs[0].props.bold && !post.runs[1].props.bold);
    let prefs = ls[3];
    assert!(!prefs.runs[0].props.underline);
    assert_eq!(prefs.runs[0].props.color.as_deref(), Some("222222"));
}

#[test]
fn links_survive_package_save_and_load() {
    let mut none = |_: &str, _: usize| None;
    let imported = import_html_with(&fixture("links.html"), &mut none).unwrap();
    let saved = save_package(&imported.package());
    let back = load_package(&saved).unwrap();
    assert_eq!(summary(&back.document), expected());
    // Every URL is an external relationship of the main part; the anchor
    // link is `w:anchor`, with none.
    let rels =
        String::from_utf8(back.part("word/_rels/document.xml.rels").unwrap().to_vec()).unwrap();
    for (to, _) in expected().iter().filter(|(to, _)| !to.starts_with('#')) {
        let target = format!("Target=\"{}\"", to.replace('&', "&amp;"));
        let rel = rels
            .split("<Relationship ")
            .find(|r| r.contains(&target))
            .unwrap_or_else(|| panic!("no relationship for {to}: {rels}"));
        assert!(rel.contains("TargetMode=\"External\""), "{rel}");
        assert!(rel.contains("/hyperlink\""), "{rel}");
    }
    let doc_xml = String::from_utf8(back.part("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(
        doc_xml.contains("<w:hyperlink w:anchor=\"top\">"),
        "{doc_xml}"
    );
    let anchor = links(&back.document)
        .into_iter()
        .find(|h| h.anchor.is_some())
        .unwrap();
    assert_eq!(
        (anchor.target.as_deref(), anchor.rel_id.as_deref()),
        (None, None)
    );
}
