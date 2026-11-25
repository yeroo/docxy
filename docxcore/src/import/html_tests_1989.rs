//! An HTML page's ordered lists (#1989): each `<ol>` its own count, from
//! its `start`, an `<li value>` its own number, as a browser numbers them;
//! each such list its own `w:num` on its own `w:abstractNum`.

use super::*;
use crate::model::{Block as DocBlock, Paragraph};
use crate::numbering::{Numbering, compute_markers, parse_numbering_xml};

/// `html` imported through its own package.
fn package(html: &str) -> Package {
    import_html_with(html.as_bytes(), &mut |_, _| None)
        .unwrap()
        .package()
}

fn numbering(pkg: &Package) -> Numbering {
    parse_numbering_xml(&pkg.part_text("word/numbering.xml").unwrap())
}

/// The body's paragraphs, in order.
fn paras(d: &Document) -> Vec<&Paragraph> {
    d.body
        .iter()
        .filter_map(|b| match b {
            DocBlock::Paragraph(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// Each list item's text and marker, in order.
fn markers(pkg: &Package) -> Vec<(String, String)> {
    let m = compute_markers(&pkg.document, &numbering(pkg));
    pkg.document
        .body
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            DocBlock::Paragraph(p) => m.get(&vec![i]).map(|k| (p.plain_text(), k.clone())),
            _ => None,
        })
        .collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn num_of(d: &Document, text: &str) -> i32 {
    paras(d)
        .into_iter()
        .find(|p| p.plain_text() == text)
        .and_then(|p| p.props.num_id)
        .unwrap()
}

#[test]
fn each_ol_counts_on_its_own() {
    for html in [
        "<ol><li>a</ol><p>x</p><ol><li>b</ol>",
        "<ol><li>a</ol><ol><li>b</ol>",
    ] {
        let pkg = package(html);
        let num = numbering(&pkg);
        let (a, b) = (num_of(&pkg.document, "a"), num_of(&pkg.document, "b"));
        assert_ne!(a, b, "{html}");
        assert_ne!(num.abstract_of(a), num.abstract_of(b), "{html}");
        assert_eq!(markers(&pkg), pairs(&[("a", "1."), ("b", "1.")]), "{html}");
    }
}

#[test]
fn an_ol_start_is_its_first_number() {
    let pkg = package("<ol start=5><li>a<li>b</ol>");
    assert_eq!(markers(&pkg), pairs(&[("a", "5."), ("b", "6.")]));
    let id = num_of(&pkg.document, "a");
    assert_eq!(numbering(&pkg).level(id, 0).unwrap().start, 5);
    let xml = pkg.part_text("word/numbering.xml").unwrap();
    assert!(
        xml.contains("<w:lvl w:ilvl=\"0\"><w:start w:val=\"5\"/>"),
        "{xml}"
    );
}

#[test]
fn the_hang_is_as_wide_as_the_items_own_number() {
    let hang = |html: &str, text: &str| {
        let pkg = package(html);
        paras(&pkg.document)
            .into_iter()
            .find(|p| p.plain_text() == text)
            .unwrap()
            .props
            .first_line
    };
    let one_digit = hang("<ol><li>a</ol>", "a");
    let two_digits = hang("<ol start=10><li>a</ol>", "a");
    assert!(two_digits < one_digit, "{two_digits} {one_digit}");
    let page = "<ol start=9><li>nine<li>ten</ol>";
    assert_eq!(hang(page, "nine"), one_digit);
    assert_eq!(hang(page, "ten"), two_digits);
    assert_eq!(hang("<ol><li>a<li value=10>ten</ol>", "ten"), two_digits);
}

#[test]
fn an_li_value_starts_a_count_of_its_own() {
    let pkg = package("<ol><li>a<li value=7>b<li>c</ol>");
    assert_eq!(
        markers(&pkg),
        pairs(&[("a", "1."), ("b", "7."), ("c", "8.")])
    );
    let d = &pkg.document;
    assert_ne!(num_of(d, "a"), num_of(d, "b"));
    assert_eq!(num_of(d, "b"), num_of(d, "c"));
    // A value that is the running count changes nothing.
    let pkg = package("<ol><li value=1>a<li value=2>b</ol>");
    assert_eq!(markers(&pkg), pairs(&[("a", "1."), ("b", "2.")]));
    assert_eq!(
        (num_of(&pkg.document, "a"), num_of(&pkg.document, "b")),
        (2, 2)
    );
    // A first item's value is the list's start: one list, not an empty one.
    let got = import_html_with(b"<ol><li value=3>a<li>b</ol>", &mut |_, _| None).unwrap();
    assert_eq!(got.lists.len(), 1);
    assert_eq!(got.lists[0].start, 3);
    let pkg = got.package();
    assert_eq!(markers(&pkg), pairs(&[("a", "3."), ("b", "4.")]));
}

#[test]
fn a_nested_ol_counts_on_its_own_and_its_parent_goes_on() {
    let pkg = package("<ol><li>a<ol start=3><li>x<li>y</ol><li>b</ol>");
    assert_eq!(
        markers(&pkg),
        pairs(&[("a", "1."), ("x", "3."), ("y", "4."), ("b", "2.")])
    );
    let d = &pkg.document;
    assert_eq!(num_of(d, "a"), num_of(d, "b"));
    assert_ne!(num_of(d, "a"), num_of(d, "x"));
    let x = paras(d)
        .into_iter()
        .find(|p| p.plain_text() == "x")
        .unwrap();
    assert_eq!(x.props.ilvl, 1);
    assert_eq!(numbering(&pkg).level(num_of(d, "x"), 1).unwrap().start, 3);
}

#[test]
fn a_page_of_one_plain_ol_is_as_before() {
    let got = import_html_with(b"<ol><li>a<li>b</ol><ul><li>c</ul>", &mut |_, _| None).unwrap();
    assert!(got.lists.is_empty());
    let pkg = got.package();
    let fresh = crate::package::new_markdown_package(Document::default());
    assert_eq!(
        pkg.part_text("word/numbering.xml"),
        fresh.part_text("word/numbering.xml")
    );
    let d = &pkg.document;
    assert_eq!((num_of(d, "a"), num_of(d, "b"), num_of(d, "c")), (2, 2, 1));
}

#[test]
fn a_bare_document_folds_its_lists_into_one_count() {
    let page = "<ol start=5><li>a</ol><ol><li>b<li value=9>c</ol><ul><li>d</ul>\
                <table><tr><td><ol start=4><li>e</ol></td></tr></table>";
    let ids = |d: &Document| {
        let mut out = Vec::new();
        fn walk(blocks: &[DocBlock], out: &mut Vec<i32>) {
            for b in blocks {
                match b {
                    DocBlock::Paragraph(p) => out.extend(p.props.num_id),
                    DocBlock::Table(t) => {
                        for row in &t.rows {
                            for cell in &row.cells {
                                walk(&cell.blocks, out);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        walk(&d.body, &mut out);
        out
    };
    // The page's own import has lists of their own, a cell's too.
    let own = import_html_with(page.as_bytes(), &mut |_, _| None).unwrap();
    assert_eq!(own.lists.len(), 3);
    assert!(ids(&own.document).iter().any(|&n| n > 2));
    let doc = import_html(page.as_bytes()).unwrap();
    assert_eq!(ids(&doc), [2, 2, 2, 1, 2]);
    // The clipboard's import (`Clip::from_html`'s).
    let pasted = import_html_str_within(page, &Budget::standard()).unwrap();
    assert_eq!(ids(&pasted), [2, 2, 2, 1, 2]);
    // And a caller's own fold.
    let mut own = own;
    own.fold_lists();
    assert!(own.lists.is_empty());
    assert_eq!(ids(&own.document), [2, 2, 2, 1, 2]);
}

#[test]
fn the_mail_export_writes_each_list_with_its_start() {
    let ols = |html: &str| -> Vec<String> {
        let pkg = package(html);
        let h = crate::export_email::to_email_html(&pkg.document, Some(&pkg)).html;
        h.match_indices("<ol")
            .map(|(at, _)| h[at..at + h[at..].find('>').unwrap() + 1].to_string())
            .collect()
    };
    let starts = |html: &str| -> Vec<Option<String>> {
        ols(html)
            .iter()
            .map(|t| {
                t.split("start=\"")
                    .nth(1)
                    .map(|r| r[..r.find('"').unwrap()].to_string())
            })
            .collect()
    };
    let s = |v: &str| Some(v.to_string());
    assert_eq!(starts("<ol start=5><li>a<li>b</ol>"), [s("5")]);
    assert_eq!(starts("<ol><li>a</ol><p>x</p><ol><li>b</ol>"), [None, None]);
    assert_eq!(starts("<ol><li>a</ol><ol><li>b</ol>"), [None, None]);
    assert_eq!(starts("<ol><li>a<li value=7>b<li>c</ol>"), [None, s("7")]);
}

#[test]
fn start_and_value_are_read_as_a_browser_reads_integers() {
    for (v, want) in [
        ("abc", None),
        ("", None),
        ("-", None),
        ("5abc", Some(5)),
        (" +7", Some(7)),
        ("\t\n12 ", Some(12)),
        ("-4", Some(0)),
        ("0", Some(0)),
        ("99999999", Some(32_767)),
        ("99999999999999999999999", Some(32_767)),
    ] {
        assert_eq!(list_number(v), want, "{v:?}");
    }
    let first = |html: &str| markers(&package(html))[0].1.clone();
    assert_eq!(first("<ol start=abc><li>a</ol>"), "1.");
    assert_eq!(first("<ol start=5abc><li>a</ol>"), "5.");
    assert_eq!(first("<ol start=\" +7\"><li>a</ol>"), "7.");
    assert_eq!(first("<ol start=-4><li>a</ol>"), "0.");
    assert_eq!(first("<ol><li value=99999999>a</ol>"), "32767.");
    // A `ul`'s start and an item's value in it are nothing.
    let pkg = package("<ul start=5><li value=3>a</ul>");
    assert_eq!(num_of(&pkg.document, "a"), 1);
}

#[test]
fn past_the_cap_a_list_is_the_markdown_list_again() {
    let page: String = (0..300).map(|i| format!("<ol><li>i{i}</ol>")).collect();
    let got = import_html_with(page.as_bytes(), &mut |_, _| None).unwrap();
    assert_eq!(got.lists.len(), MAX_HTML_LISTS);
    let pkg = got.package();
    let d = &pkg.document;
    assert_eq!(num_of(d, "i0"), 2);
    assert_eq!(num_of(d, "i1"), 3);
    assert_eq!(num_of(d, "i256"), 258);
    assert_eq!(num_of(d, "i257"), 2);
    assert_eq!(num_of(d, "i299"), 2);
    let xml = pkg.part_text("word/numbering.xml").unwrap();
    assert_eq!(xml.matches("<w:num ").count(), 2 + MAX_HTML_LISTS);
    assert_eq!(xml.matches("<w:abstractNum ").count(), 2 + MAX_HTML_LISTS);
}

#[test]
fn add_html_lists_refuses_ids_the_part_has() {
    let mut pkg = crate::package::new_markdown_package(Document::default());
    let before = pkg.part_text("word/numbering.xml");
    // numId 2 and abstract 101 are the Markdown part's own.
    assert!(!pkg.add_html_lists(&[(2, 102, 0, 5)]));
    assert!(!pkg.add_html_lists(&[(3, 101, 0, 5)]));
    assert_eq!(pkg.part_text("word/numbering.xml"), before);
    assert!(pkg.add_html_lists(&[]));
    assert!(pkg.add_html_lists(&[(3, 102, 1, 4)]));
    let num = numbering(&pkg);
    assert_eq!(num.abstract_of(3), Some(102));
    assert_eq!(num.level(3, 0).unwrap().start, 1);
    assert_eq!(num.level(3, 1).unwrap().start, 4);
    // A package with no numbering part gets the Markdown one first.
    let mut bare = crate::package::new_package(Document::default());
    assert!(bare.add_html_lists(&[(3, 102, 0, 2)]));
    let num = numbering(&bare);
    assert!(num.defines(1) && num.defines(2) && num.defines(3));
}
