//! HTML import rounded images and cards (#1926): a team digest with round
//! 32 px avatars (`border-radius: 50%` inline, through a class, and as
//! 16 px), a header picture with 8 px corners, cards (a cell, a `div`, a
//! `span`) with `border-radius` and a background. Each picture keeps its
//! crop as Word's Crop to Shape writes it (`a:prstGeom`), through save and
//! load; a card's `border-radius` is dropped, never text, and its shading
//! and padding stay.

use docxcore::image::{PictureShape, inline_embed, inline_shape};
use docxcore::import::import_html_with;
use docxcore::model::{Block, Document, Inline, Table};
use docxcore::package::{load_package, save_package};
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

fn digest() -> docxcore::import::HtmlImport {
    let mut resolve = |_: &str, _: usize| Some(png(64, 64));
    import_html_with(&fixture("digest-rounded.html"), &mut resolve).unwrap()
}

/// Every picture run in the blocks, cells included, in order.
fn pictures(blocks: &[Block]) -> Vec<&str> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => out.extend(p.content.iter().filter_map(|i| match i {
                Inline::Raw(r) if inline_embed(r).is_some() => Some(r.as_str()),
                _ => None,
            })),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        out.extend(pictures(&cell.blocks));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn shapes(d: &Document) -> Vec<PictureShape> {
    pictures(&d.body).iter().map(|r| inline_shape(r)).collect()
}

/// Header 8 of 40 px; Ann, Bob and Cy round; the chart plain; the tab with
/// one rounded corner a rectangle.
const EXPECTED: [PictureShape; 6] = [
    PictureShape::RoundRect { adj: 20_000 },
    PictureShape::Ellipse,
    PictureShape::Ellipse,
    PictureShape::Ellipse,
    PictureShape::Rect,
    PictureShape::Rect,
];

#[test]
fn each_picture_keeps_its_crop_shape() {
    let got = digest();
    assert_eq!(shapes(&got.document), EXPECTED);
    let runs = pictures(&got.document.body);
    assert!(runs[1].contains(r#"<a:prstGeom prst="ellipse"><a:avLst/></a:prstGeom>"#));
    assert!(!runs[1].contains(r#"prst="rect""#));
    assert!(runs[0].contains(r#"<a:gd name="adj" fmla="val 20000"/>"#));
}

#[test]
fn the_crop_shapes_survive_save_and_load() {
    let pkg = digest().package();
    let back = load_package(&save_package(&pkg)).unwrap();
    assert_eq!(shapes(&back.document), EXPECTED);
}

fn table(d: &Document) -> &Table {
    d.body
        .iter()
        .find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .expect("the digest table")
}

/// Regression guard (passes before #1926 too): a card's `border-radius`
/// is dropped; its background and padding import as before, and no radius
/// value shows as text.
#[test]
fn a_cards_border_radius_is_never_text_and_its_shading_stays() {
    let got = digest();
    let t = table(&got.document);
    let tcpr = |row: usize| t.rows[row].cells[0].raw_tcpr.clone().unwrap_or_default();
    assert!(tcpr(1).contains(r#"w:fill="F4F6F8""#), "{}", tcpr(1));
    assert!(tcpr(1).contains("<w:tcMar>"), "{}", tcpr(1));
    assert!(tcpr(2).contains(r#"w:fill="FFF8E1""#), "{}", tcpr(2));
    // The div card: paragraph shading in its cell.
    let div_shaded = t.rows[3].cells[0].blocks.iter().any(|b| match b {
        Block::Paragraph(p) => p.props.raw_props.iter().any(|r| r.contains("E8F5E9")),
        _ => false,
    });
    assert!(div_shaded, "{:?}", t.rows[3].cells[0].blocks);
    let text = docxcore::import::paragraph_texts(&got.document).join("\n");
    let cells: Vec<String> = t
        .rows
        .iter()
        .flat_map(|r| r.cells.iter())
        .flat_map(|c| c.blocks.iter())
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(p.plain_text()),
            _ => None,
        })
        .collect();
    let all = format!("{text}\n{}", cells.join("\n"));
    assert!(all.contains("Ann posted 3 new updates."), "{all}");
    for junk in ["radius", "50%", "8px", "16px", "12px"] {
        assert!(!all.contains(junk), "{junk} in {all}");
    }
}
