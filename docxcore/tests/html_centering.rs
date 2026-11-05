//! HTML import centring (#1922): a digest mail that centres its content
//! the ways e-mail does (a `<td align=center>` around a 600 px card, a
//! `margin: 0 auto` table, `<center>`, block images, a fixed-width
//! `margin: 0 auto` box) imports with centred tables (`w:jc`), centred
//! picture paragraphs and evenly indented boxes, as a browser places them.

use docxcore::image::inline_embed;
use docxcore::import::import_html_with;
use docxcore::model::{Align, Block, Document, Inline, Paragraph, Table};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/html")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A PNG header of `w` x `h` pixels: what `image::sniff` reads.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    out.extend(w.to_be_bytes());
    out.extend(h.to_be_bytes());
    out.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// The digest, its images resolved.
fn digest() -> Document {
    let mut resolve = |_: &str, _: usize| Some(png(120, 40));
    import_html_with(&fixture("digest-centering.html"), &mut resolve)
        .unwrap()
        .document
}

fn top_tables(d: &Document) -> Vec<&Table> {
    d.body
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn nested(t: &Table, row: usize, cell: usize) -> Vec<&Table> {
    t.rows[row].cells[cell]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn cell_paras(t: &Table, row: usize, cell: usize) -> Vec<&Paragraph> {
    t.rows[row].cells[cell]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn body_paras(d: &Document) -> Vec<&Paragraph> {
    d.body
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(p),
            _ => None,
        })
        .collect()
}

fn jc(t: &Table) -> Option<String> {
    let pr = t.raw_tblpr.as_deref().unwrap_or("");
    let v = pr.split("<w:jc w:val=\"").nth(1)?;
    Some(v[..v.find('"')?].to_string())
}

fn has_picture(p: &Paragraph) -> bool {
    p.content
        .iter()
        .any(|i| matches!(i, Inline::Raw(r) if inline_embed(r).is_some()))
}

/// (text, holds a picture, alignment) of each paragraph.
fn sketch(ps: &[&Paragraph]) -> Vec<(String, bool, Align)> {
    ps.iter()
        .map(|p| (p.plain_text(), has_picture(p), p.props.align))
        .collect()
}

#[test]
fn a_digest_keeps_its_centring() {
    let d = digest();
    let tables = top_tables(&d);
    assert_eq!(tables.len(), 4, "wrapper, card, center, plain");

    // The 100% wrapper sits where the body puts it; the 600 px card in its
    // `align=center` cell is centred.
    let wrapper = tables[0];
    assert_eq!(jc(wrapper), None);
    let card = nested(wrapper, 0, 0);
    assert_eq!(card.len(), 1);
    let card = card[0];
    assert_eq!(jc(card), Some("center".into()));

    // The card's first cell inherits the centring: its logo (a block image
    // by the page's `img {display: block}`, in a link) is a centred
    // paragraph of its own; the heading's own `text-align: left` and the
    // inherited centre of the text stay as they were.
    let first = cell_paras(card, 0, 0);
    assert_eq!(
        sketch(&first),
        [
            (String::new(), true, Align::Center),
            ("Your weekly digest".into(), false, Align::Left),
            ("Three new posts this week.".into(), false, Align::Center),
        ]
    );

    // `align=left|right` tables are floats: their own place, whatever the
    // container says.
    let columns = nested(card, 1, 0);
    assert_eq!(
        columns.iter().map(|t| jc(t)).collect::<Vec<_>>(),
        [None, Some("right".into())]
    );

    // `.card { margin: 0 auto }` and `<center>` centre theirs; a plain
    // `text-align: center` does not move a table.
    assert_eq!(jc(tables[1]), Some("center".into()));
    assert_eq!(jc(tables[2]), Some("center".into()));
    assert_eq!(jc(tables[3]), None);

    let body = body_paras(&d);
    let find = |text: &str| {
        *body
            .iter()
            .find(|p| p.plain_text() == text)
            .unwrap_or_else(|| panic!("no {text:?} in {:?}", sketch(&body)))
    };
    assert_eq!(find("Centred text.").props.align, Align::Center);

    // The hero image: its own centred paragraph; the text after it is left.
    let pictures: Vec<&&Paragraph> = body.iter().filter(|p| has_picture(p)).collect();
    assert_eq!(pictures.len(), 1, "{:?}", sketch(&body));
    assert_eq!(pictures[0].props.align, Align::Center);
    assert_eq!(pictures[0].plain_text(), "");
    assert_eq!(find("After the hero.").props.align, Align::Left);

    // The 300 px note with 20 px side padding, centred: (9360 - 4500 -
    // 2 × 300) / 2 + 300 on each side; its text keeps its alignment.
    let note = find("A note in a 300 px box, centred on the page.");
    assert_eq!((note.props.indent, note.props.indent_right), (2430, 2430));
    assert_eq!(note.props.align, Align::Left);
}
