//! HTML import images (#1880): a mail with `cid:` images, a `data:` URI, a
//! refused remote image with and without a size, a hidden tracking pixel
//! and a repeated src, read through a resolver, then saved in a package and
//! loaded back.

use docxcore::image::{inline_embed, inline_extent};
use docxcore::import::{HtmlImport, import_html, import_html_with, paragraph_texts};
use docxcore::model::{Block, Document, Inline};
use docxcore::package::{Package, load_package, save_package};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/html")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A PNG header of `w` x `h` pixels (signature and IHDR, its CRC right):
/// what `image::sniff` reads.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend(13u32.to_be_bytes());
    out.extend(&ihdr);
    out.extend(opccore::zipwrite::crc32(&ihdr).to_be_bytes());
    out
}

/// The PNG the fixture's `data:` URI holds (4 x 2, red).
const DATA_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAIAAADwyuo0AAAAEElEQVR42mM4IScHRwzIHABvCggh7qR9+QAAAABJRU5ErkJggg==";

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let (mut acc, mut bits, mut out) = (0u32, 0, Vec::new());
    for c in s.bytes().filter(|&c| c != b'=') {
        acc = (acc << 6) | u32::from(val(c));
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// The fixture through a resolver that knows the two `cid:` images and
/// refuses everything else, with the srcs it was asked for.
fn import_message() -> (HtmlImport, Vec<String>) {
    let mut asked = Vec::new();
    let mut resolve = |src: &str, limit: usize| {
        asked.push(src.to_string());
        assert!(limit > 0);
        match src {
            "cid:logo@example.com" => Some(png(40, 20)),
            "cid:banner@example.com" => Some(png(30, 10)),
            _ => None,
        }
    };
    let got = import_html_with(&fixture("cid-message.html"), &mut resolve).unwrap();
    (got, asked)
}

/// Every picture run of `blocks`, tables (nested too) in reading order.
fn pictures(blocks: &[Block]) -> Vec<&str> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                for i in &p.content {
                    if let Inline::Raw(raw) = i {
                        if inline_embed(raw).is_some() {
                            out.push(raw.as_str());
                        }
                    }
                }
            }
            Block::Table(t) => {
                for cell in t.rows.iter().flat_map(|r| &r.cells) {
                    out.extend(pictures(&cell.blocks));
                }
            }
            _ => {}
        }
    }
    out
}

/// `wp:docPr`'s `descr`, as written.
fn descr(raw: &str) -> Option<&str> {
    let at = raw.find(" descr=\"")? + 8;
    Some(&raw[at..at + raw[at..].find('"')?])
}

const PX: i64 = 9525;

#[test]
fn the_resolver_is_asked_for_each_other_src_once_never_a_data_or_hidden_one() {
    let (_, asked) = import_message();
    assert_eq!(
        asked,
        [
            "cid:logo@example.com",
            "cid:banner@example.com",
            "http://tracker.example.com/map.png",
            "https://images.example.com/missing.png",
        ]
    );
}

#[test]
fn pictures_take_their_declared_size_the_aspect_or_their_pixels() {
    let (got, _) = import_message();
    let pics = pictures(&got.document.body);
    let sizes: Vec<(i64, i64)> = pics.iter().map(|r| inline_extent(r).unwrap()).collect();
    assert_eq!(
        sizes,
        [
            // `.logo { width: 120px }`, its height by the 40 x 20 aspect.
            (120 * PX, 60 * PX),
            // In the nested table, width and height given.
            (300 * PX, 100 * PX),
            // The `data:` image, unsized: its 4 x 2 pixels.
            (4 * PX, 2 * PX),
            // The refused remote image: a placeholder of its stated size.
            (200 * PX, 50 * PX),
            // The logo again at `width="60"`.
            (60 * PX, 30 * PX),
            // `width="150"` beside `width:100%` and `height:auto`.
            (150 * PX, 50 * PX),
        ]
    );
    let descrs: Vec<Option<&str>> = pics.iter().map(|r| descr(r)).collect();
    assert_eq!(
        descrs,
        [
            Some("Example Shop"),
            Some("Autumn sale"),
            Some("Parcel chart"),
            Some("Delivery map"),
            None,
            None,
        ]
    );
    // Only the placeholder is outlined.
    let outlined: Vec<bool> = pics.iter().map(|r| r.contains("<a:ln ")).collect();
    assert_eq!(outlined, [false, false, false, true, false, false]);
    // A repeated src is one medium.
    let rid = |i: usize| inline_embed(pics[i]).unwrap();
    assert_eq!(rid(0), rid(4));
    assert_eq!(rid(1), rid(5));
    assert_eq!(
        got.media.len(),
        4,
        "logo, banner, the data image, the placeholder"
    );
}

#[test]
fn an_unsized_refused_image_is_its_alt_text_and_a_hidden_one_is_nothing() {
    let (got, _) = import_message();
    let texts = paragraph_texts(&got.document);
    assert!(texts.iter().any(|t| t == "Shop front"), "{texts:?}");
    assert!(
        texts.contains(&"Track it here:  or call us.".to_string()),
        "{texts:?}"
    );
    assert!(texts.contains(&"Thanks, ".to_string()), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("Delivery map")));
}

#[test]
fn the_data_image_is_embedded_as_decoded() {
    let (got, _) = import_message();
    let pics = pictures(&got.document.body);
    let rid = inline_embed(pics[2]).unwrap();
    let medium = got.media.iter().find(|m| m.rid == rid).unwrap();
    assert_eq!(medium.bytes, base64(DATA_PNG));
    assert_eq!((medium.info.width, medium.info.height), (4, 2));
}

/// Each picture's bytes through `word/document.xml`'s relationships.
fn media_of(pkg: &Package, doc: &Document) -> Vec<Vec<u8>> {
    pictures(&doc.body)
        .iter()
        .map(|raw| {
            let rid = inline_embed(raw).unwrap();
            let part = pkg
                .part_rel_target("word/document.xml", &rid)
                .unwrap_or_else(|| panic!("{rid} resolves"));
            assert!(part.starts_with("word/media/image"), "{part}");
            pkg.part(&part).unwrap().to_vec()
        })
        .collect()
}

#[test]
fn the_package_holds_each_medium_and_keeps_them_through_save_and_load() {
    let (got, _) = import_message();
    let pkg = got.package();
    let expected = vec![
        png(40, 20),
        png(30, 10),
        base64(DATA_PNG),
        // The placeholder: a 1 x 1 PNG.
        media_of(&pkg, &pkg.document)[3].clone(),
        png(40, 20),
        png(30, 10),
    ];
    assert_eq!(media_of(&pkg, &pkg.document), expected);
    let placeholder = docxcore::image::sniff(&expected[3]).unwrap();
    assert_eq!((placeholder.width, placeholder.height), (1, 1));
    let types = String::from_utf8(pkg.part("[Content_Types].xml").unwrap().to_vec()).unwrap();
    assert!(types.contains("Extension=\"png\""), "{types}");
    // Four media parts, no import-local rid left.
    let saved = save_package(&pkg);
    let back = load_package(&saved).unwrap();
    assert_eq!(media_of(&back, &back.document), expected);
    let doc_xml = String::from_utf8(back.part("word/document.xml").unwrap().to_vec()).unwrap();
    assert!(!doc_xml.contains("rIdHtmlImage"));
    assert!(doc_xml.contains("descr=\"Delivery map\""));
    let media: Vec<&str> = back
        .part_names()
        .into_iter()
        .filter(|n| n.starts_with("word/media/"))
        .collect();
    assert_eq!(media.len(), 4, "{media:?}");
}

#[test]
fn import_html_without_a_resolver_converts_no_image() {
    let doc = import_html(&fixture("cid-message.html")).unwrap();
    assert!(pictures(&doc.body).is_empty());
    let texts = paragraph_texts(&doc);
    assert!(!texts.iter().any(|t| t == "Shop front"));
}

#[test]
fn a_page_of_only_an_image_imports_it() {
    let page =
        format!("<html><body><p><img src=\"data:image/png;base64,{DATA_PNG}\"></p></body></html>");
    let mut none = |_: &str, _: usize| None;
    let got = import_html_with(page.as_bytes(), &mut none).unwrap();
    assert_eq!(pictures(&got.document.body).len(), 1);
    // Without images it is an empty paragraph, as before.
    let doc = import_html(page.as_bytes()).unwrap();
    assert_eq!(paragraph_texts(&doc), [""]);
}

#[test]
fn the_message_cut_anywhere_never_panics() {
    let page = fixture("cid-message.html");
    for i in 0..page.len() {
        let mut resolve =
            |src: &str, _: usize| (src == "cid:logo@example.com").then(|| png(40, 20));
        if let Ok(got) = import_html_with(&page[..i], &mut resolve) {
            let _ = got.package();
        }
    }
}
