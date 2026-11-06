//! HTML import nested cards (#1927): a weekly digest with padded, shaded
//! cells and `div` cards nested three levels deep. Each level keeps its own
//! background as its cell's `w:shd` and its padding as its `w:tcMar`, a
//! `div` card as a one-cell table of its own, through save and load.

use docxcore::import::import_html_with;
use docxcore::model::{Block, Document, Table};
use docxcore::package::{load_package, save_package};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/html")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn digest() -> docxcore::import::HtmlImport {
    let mut resolve = |_: &str, _: usize| None;
    import_html_with(&fixture("digest-cards.html"), &mut resolve).unwrap()
}

fn tables(blocks: &[Block]) -> Vec<&Table> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn texts(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.plain_text(),
            Block::Table(_) => "[table]".into(),
            _ => "?".into(),
        })
        .collect()
}

fn tcpr(t: &Table, row: usize) -> &str {
    t.rows[row].cells[0].raw_tcpr.as_deref().unwrap_or("")
}

fn blocks(t: &Table, row: usize) -> &[Block] {
    &t.rows[row].cells[0].blocks
}

/// `w:tcMar` of top, left, bottom, right twips.
fn tcmar(top: u32, left: u32, bottom: u32, right: u32) -> String {
    format!(
        "<w:tcMar><w:top w:w=\"{top}\" w:type=\"dxa\"/><w:left w:w=\"{left}\" w:type=\"dxa\"/>\
         <w:bottom w:w=\"{bottom}\" w:type=\"dxa\"/><w:right w:w=\"{right}\" w:type=\"dxa\"/></w:tcMar>"
    )
}

fn fill(rgb: &str) -> String {
    format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{rgb}\"/>")
}

/// Every level of the digest, as the importer and a save and load give it.
fn check_levels(d: &Document) {
    let outer = tables(&d.body)[0];
    // Level 1: the white `td`, padded 24 px.
    let pr = tcpr(outer, 0);
    assert!(pr.contains(&fill("FFFFFF")), "{pr}");
    assert!(pr.contains(&tcmar(360, 360, 360, 360)), "{pr}");
    let cell = blocks(outer, 0);
    assert_eq!(
        texts(cell),
        ["Your week", "[table]", "See you next week."],
        "{cell:?}"
    );

    // Level 2: the `.card` div, its own one-cell table.
    let card = tables(cell)[0];
    assert_eq!((card.rows.len(), card.rows[0].cells.len()), (1, 1));
    let pr = tcpr(card, 0);
    assert!(pr.contains(&fill("F1F3F5")), "{pr}");
    assert!(pr.contains(&tcmar(240, 240, 240, 240)), "{pr}");
    // Laid out in the white cell's width less its padding.
    assert_eq!(card.grid.iter().sum::<u32>(), 9000 - 720);
    let in_card = blocks(card, 0);
    assert_eq!(
        texts(in_card),
        ["Three projects moved this week.", "[table]", ""]
    );

    // Level 3: the table in the card, laid out in the card's content box.
    let list = tables(in_card)[0];
    assert_eq!(list.grid.iter().sum::<u32>(), 9000 - 720 - 480);
    let pr = tcpr(list, 0);
    assert!(pr.contains(&fill("DDDDDD")), "{pr}");
    assert!(pr.contains(&tcmar(120, 180, 120, 180)), "{pr}");
    // Two item cards; the empty divider between them makes nothing, and
    // they stay apart by one empty paragraph.
    let items = blocks(list, 0);
    assert_eq!(texts(items), ["[table]", "", "[table]", ""], "{items:?}");
    for (item, text) in tables(items).into_iter().zip(["Atlas", "Borealis"]) {
        let pr = tcpr(item, 0);
        assert!(pr.contains(&fill("FAFAFA")), "{pr}");
        assert!(pr.contains(&tcmar(150, 150, 150, 150)), "{pr}");
        assert!(texts(blocks(item, 0))[0].starts_with(text));
    }

    // The `#CCC` cell and the `#BBB` cell nested in it.
    let pr = tcpr(list, 1);
    assert!(pr.contains(&fill("CCCCCC")), "{pr}");
    assert!(pr.contains(&tcmar(60, 60, 60, 60)), "{pr}");
    let comet = tables(blocks(list, 1))[0];
    let pr = tcpr(comet, 0);
    assert!(pr.contains(&fill("BBBBBB")), "{pr}");
    assert!(pr.contains(&tcmar(90, 90, 30, 90)), "{pr}");
}

#[test]
fn each_level_keeps_its_fill_and_padding() {
    check_levels(&digest().document);
}

/// Regression guard (passes before #1927 too): nested `td` cards keep their
/// own fill and padding.
#[test]
fn nested_td_cards_keep_fill_and_padding() {
    let got = digest();
    let outer = tables(&got.document.body)[0];
    assert!(tcpr(outer, 0).contains(&fill("FFFFFF")));
    assert!(tcpr(outer, 0).contains(&tcmar(360, 360, 360, 360)));
    let html = "<table><tr><td bgcolor=#CCCCCC style='padding:4px'>\
                <table><tr><td style='background-color:#BBBBBB;padding:6px 6px 2px 6px'>x\
                </td></tr></table></td></tr></table>";
    let d = docxcore::import::import_html(html.as_bytes()).unwrap();
    let outer = tables(&d.body)[0];
    assert!(tcpr(outer, 0).contains(&fill("CCCCCC")));
    assert!(tcpr(outer, 0).contains(&tcmar(60, 60, 60, 60)));
    let inner = tables(blocks(outer, 0))[0];
    assert!(tcpr(inner, 0).contains(&fill("BBBBBB")));
    assert!(tcpr(inner, 0).contains(&tcmar(90, 90, 30, 90)));
}

#[test]
fn cards_survive_save_and_reload() {
    let pkg = digest().package();
    let back = load_package(&save_package(&pkg)).unwrap();
    check_levels(&back.document);
}
