//! The images leftovers of #1880 (#1898): percentage sizes and
//! `max-width` / `max-height`, `<base href>`, `srcset` and `<picture>`,
//! MHTML, SVG and WebP.

use super::*;
use crate::image::inline_extent;
use crate::import::paragraph_texts;

/// A PNG header of `w` x `h` pixels: enough for `image::sniff`.
pub(super) fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    out.extend(w.to_be_bytes());
    out.extend(h.to_be_bytes());
    out.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// `html` imported with a resolver giving a 40 x 20 PNG for any src but
/// `missing…`: the import and the srcs asked for.
fn import(html: &str) -> (HtmlImport, Vec<String>) {
    let mut asked = Vec::new();
    let mut resolve = |src: &str, _: usize| {
        asked.push(src.to_string());
        (!src.contains("missing")).then(|| png(40, 20))
    };
    let got = import_html_within(html.as_bytes(), &Budget::standard(), Some(&mut resolve)).unwrap();
    (got, asked)
}

/// Every picture run of `blocks`, tables (nested too) in reading order.
pub(super) fn pictures(blocks: &[DocBlock]) -> Vec<&str> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            DocBlock::Paragraph(p) => {
                for i in &p.content {
                    if let Inline::Raw(raw) = i {
                        if inline_extent(raw).is_some() {
                            out.push(raw.as_str());
                        }
                    }
                }
            }
            DocBlock::Table(t) => {
                for cell in t.rows.iter().flat_map(|r| &r.cells) {
                    out.extend(pictures(&cell.blocks));
                }
            }
            _ => {}
        }
    }
    out
}

/// Each picture's extent, in twips.
fn twips(html: &str) -> Vec<(i64, i64)> {
    let (got, _) = import(html);
    pictures(&got.document.body)
        .iter()
        .map(|r| {
            let (cx, cy) = inline_extent(r).unwrap();
            (cx / 635, cy / 635)
        })
        .collect()
}

const BODY: i64 = html_table::BODY_TWIPS as i64;

#[test]
fn a_percentage_width_is_of_the_body_outside_a_cell_1898() {
    assert_eq!(
        twips("<p><img src=a.png width=50%><img src=a.png style='width:25%'></p>"),
        // 25% of 11790 twips rounds up.
        [(BODY / 2, BODY / 4), (2948, 1474)]
    );
    // CSS over the attribute, either way.
    assert_eq!(
        twips(
            "<p><img src=a.png width=50% style='width:300px'><img src=a.png width=300 style='width:100%'></p>"
        ),
        [(4500, 2250), (BODY, BODY / 2)]
    );
}

#[test]
fn a_percentage_width_is_of_the_room_a_blocks_indents_leave_1898() {
    // A centred `max-width` wrapper (#1922, #1894) is the room.
    assert_eq!(
        twips(
            "<div style='max-width:600px;margin:0 auto'><img src=a.png style='width:100%'></div>"
        ),
        [(9000, 4500)]
    );
    // A padded block.
    assert_eq!(
        twips("<div style='padding:0 30px'><img src=a.png width=100%></div>"),
        [(BODY - 900, (BODY - 900) / 2)]
    );
}

#[test]
fn a_percentage_width_in_a_cell_is_of_its_cells_content_1898() {
    // The cell's own width, less its padding.
    assert_eq!(
        twips(
            "<table><tr><td width=400 style='padding:0 20px'><img src=a.png width=100%></td></tr></table>"
        ),
        [(5400, 2700)]
    );
    // A cell without one: its table's.
    assert_eq!(
        twips("<table width=600><tr><td><img src=a.png width=100%></td></tr></table>"),
        [(9000, 4500)]
    );
    // A percentage cell of its table's; a percentage table of the room
    // around it: the outer cell's.
    assert_eq!(
        twips(
            "<table width=600 cellpadding=0><tr><td><table width=50%><tr><td width=50%>\
             <img src=a.png width=100%></td></tr></table></td></tr></table>"
        ),
        [(2250, 1125)]
    );
    // A block in the cell indents it.
    assert_eq!(
        twips(
            "<table width=600><tr><td><div style='margin-left:100px'><img src=a.png width=50%></div></td></tr></table>"
        ),
        [(3750, 1875)]
    );
}

#[test]
fn a_percentage_width_in_a_card_is_of_the_cards_content_1898() {
    assert_eq!(
        twips(
            "<table width=600><tr><td><div style='background:#eee;width:400px;padding:10px'>\
             <img src=a.png width=100%></div></td></tr></table>"
        ),
        [(5700, 2850)]
    );
}

#[test]
fn a_percentage_height_is_auto_1898() {
    // CSS `height:50%` clears the attribute's height: the width's aspect.
    assert_eq!(
        twips(
            "<p><img src=a.png width=100 height=100 style='height:50%'><img src=a.png width=100 height=50%></p>"
        ),
        [(1500, 750), (1500, 750)]
    );
}

#[test]
fn max_width_and_max_height_clamp_the_declared_box_1898() {
    assert_eq!(
        twips(
            "<p><img src=a.png style='max-width:20px'>\
             <img src=a.png width=100 style='max-width:50px'>\
             <img src=a.png width=100 height=100 style='max-width:50px'>\
             <img src=a.png height=50 style='max-width:50px'>\
             <img src=a.png style='max-height:10px'>\
             <img src=a.png width=100 height=100 style='max-height:20px'>\
             <img src=a.png style='width:100%;max-width:50%'>\
             <img src=a.png style='max-width:20px;max-width:none'>\
             <img src=a.png style='max-height:50%'></p>"
        ),
        [
            // Unsized: its pixels, scaled together.
            (300, 150),
            // A declared width, its height from the aspect: both scale.
            (750, 375),
            // Both declared: the width alone.
            (750, 1500),
            // A declared height, its width from the aspect: the width alone.
            (750, 750),
            (300, 150),
            (1500, 300),
            // A percentage `max-width` is of the containing width.
            (BODY / 2, BODY / 4),
            // `none` clears it; a percentage `max-height` is none.
            (600, 300),
            (600, 300),
        ]
    );
}

#[test]
fn max_width_clamps_before_the_body_fit_and_the_crop_uses_the_clamped_box_1898() {
    let (got, _) = import(
        "<p><img src=a.png width=2000 height=400 style='max-width:400px;border-radius:50px'></p>",
    );
    let pics = pictures(&got.document.body);
    assert_eq!(inline_extent(pics[0]), Some((400 * 9525, 400 * 9525)));
    // 50px of a 400 x 400 box is 12.5% of its short side: not an ellipse.
    assert_eq!(
        crate::image::inline_shape(pics[0]),
        crate::image::PictureShape::RoundRect { adj: 12500 }
    );
}

#[test]
fn max_width_bounds_a_sized_placeholder_and_leaves_an_unsized_one_1898() {
    // A refused image with one side: a square, clamped together.
    assert_eq!(
        twips("<p><img src=missing.png width=200 alt=x style='max-width:100px'></p>"),
        [(1500, 1500)]
    );
    // None: its alt text, nothing to clamp.
    let (got, _) = import("<p><img src=missing.png alt=Logo style='max-width:100px'></p>");
    assert!(pictures(&got.document.body).is_empty());
    assert_eq!(paragraph_texts(&got.document), ["Logo"]);
}

#[test]
fn a_base_href_resolves_every_src_but_a_data_one_1898() {
    let data = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAIAAAA=";
    let (_, asked) = import(&format!(
        "<html><head><base href='img/'></head><body><p><img src=a.png>\
         <img src=/root.png><img src=cid:x@y><img src='{data}'></p></body></html>"
    ));
    assert_eq!(asked, ["img/a.png", "/root.png", "cid:x@y"]);
}

#[test]
fn the_first_base_href_applies_to_an_img_before_it_1898() {
    let (_, asked) =
        import("<p><img src=a.png><base target=_top><base href='img/'><base href='other/'></p>");
    assert_eq!(asked, ["img/a.png"]);
    // In a comment or a script it is not one; `data:` and `javascript:`
    // are no base.
    let (_, asked) = import(
        "<!-- <base href='c/'> --><script>'<base href=\"s/\">'</script>\
         <base href='javascript:x'><p><img src=a.png></p>",
    );
    assert_eq!(asked, ["a.png"]);
}

#[test]
fn an_absolute_base_makes_an_absolute_src_1898() {
    let (_, asked) = import("<base href='https://example.com/mail/'><p><img src=../a.png></p>");
    assert_eq!(asked, ["https://example.com/a.png"]);
}

#[test]
fn no_base_leaves_srcs_as_they_were_1898() {
    let (_, asked) = import("<p><img src='x_files/a.png'><img src='../b.png'></p>");
    assert_eq!(asked, ["x_files/a.png", "../b.png"]);
}

#[test]
fn an_img_whose_src_gives_nothing_tries_its_srcset_1898() {
    let (got, asked) = import(
        "<p><img src=missing.png srcset='missing@3x.png 3x, a@2x.png 2x, missing@1x.png' alt=x>\
         <img srcset='b.png 2x'></p>",
    );
    assert_eq!(
        asked,
        [
            "missing.png",
            "missing@1x.png",
            "missing@3x.png",
            "a@2x.png",
            "b.png"
        ]
    );
    let pics = pictures(&got.document.body);
    // The 40 x 20 PNG at 2x is 20 x 10 CSS pixels.
    let size: Vec<_> = pics.iter().map(|r| inline_extent(r).unwrap()).collect();
    assert_eq!(size, [(20 * 9525, 10 * 9525), (20 * 9525, 10 * 9525)]);
    // Named after the candidate that gave it.
    assert!(pics[0].contains("name=\"a@2x.png\""), "{}", pics[0]);
}

#[test]
fn a_src_that_resolves_never_asks_for_a_candidate_1898() {
    let (_, asked) = import("<p><img src=a.png srcset='b.png 2x'></p>");
    assert_eq!(asked, ["a.png"]);
}

#[test]
fn a_picture_offers_its_sources_after_the_imgs_own_srcset_1898() {
    let (got, asked) = import(
        "<p><picture>\
         <source media='(min-width: 600px)' srcset='wide.png'>\
         <source type='image/avif' srcset='a.avif'>\
         <source type='image/webp; codecs=x' srcset='missing.webp'>\
         <source srcset='pic.png 2x'>\
         <img src=missing.png srcset='missing-own.png'></picture>\
         <img src=missing2.png></p>",
    );
    assert_eq!(
        asked,
        [
            "missing.png",
            "missing-own.png",
            "missing.webp",
            "pic.png",
            "missing2.png"
        ]
    );
    let pics = pictures(&got.document.body);
    assert_eq!(inline_extent(pics[0]), Some((20 * 9525, 10 * 9525)));
}

#[test]
fn a_source_outside_a_picture_is_nothing_1898() {
    let (_, asked) = import("<video><source srcset='v.png'></video><p><img src=missing.png></p>");
    assert_eq!(asked, ["missing.png"]);
}

/// `bytes` in base64, by hand, wrapped at 76 as MIME writes it.
fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |a, &b| (a << 8) | u32::from(b)) << (8 * (3 - c.len()));
        for k in 0..=c.len() {
            out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
        }
        for _ in c.len()..3 {
            out.push('=');
        }
    }
    out.as_bytes()
        .chunks(76)
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Word's "Single File Web Page": CRLF lines, a preamble, the root part
/// quoted-printable in windows-1252 (its meta saying otherwise), its
/// picture a base64 part at `doc_files/image001.png`.
fn word_mht() -> Vec<u8> {
    let page = "<html xmlns:v=3D\"urn:schemas-microsoft-com:vml\">\r\n\
                <head><meta http-equiv=3DContent-Type content=3D\"text/html; charset=3Dutf-8\"></head>\r\n\
                <body><p class=3DMsoNormal>Caf=E9 <![if !vml]><img width=3D40 height=3D20 =\r\n\
                src=3D\"doc_files/image001.png\" alt=3D\"Logo\"><![endif]><img src=3D\"doc_files/gone.png\" alt=3DGone></p></body></html>";
    format!(
        "MIME-Version: 1.0\r\n\
         Content-Type: multipart/related; boundary=\"----=_NextPart_01DA0000.12345678\"\r\n\
         \r\n\
         This document is a Single File Web Page, also known as a Web Archive file.\r\n\
         \r\n\
         ------=_NextPart_01DA0000.12345678\r\n\
         Content-Location: file:///C:/X/doc.htm\r\n\
         Content-Transfer-Encoding: quoted-printable\r\n\
         Content-Type: text/html; charset=\"windows-1252\"\r\n\
         \r\n\
         {page}\r\n\
         \r\n\
         ------=_NextPart_01DA0000.12345678\r\n\
         Content-Location: file:///C:/X/doc_files/image001.png\r\n\
         Content-Transfer-Encoding: base64\r\n\
         Content-Type: image/png\r\n\
         \r\n\
         {}\r\n\
         \r\n\
         ------=_NextPart_01DA0000.12345678--\r\n",
        b64(&png(8, 4))
    )
    .into_bytes()
}

/// Blink's "Webpage, single file": `From: <Saved by Blink>`, folded
/// top headers, an `https:` root, absolute srcs matching their parts.
fn blink_mhtml() -> Vec<u8> {
    let page = "<!DOCTYPE html><html><head><meta charset=3D\"utf-8\"></head><body>\n\
                <p>Hello <img src=3D\"https://cdn.example.com/a.png?v=3D1\"> <img src=3D\"img/b.png\">=\n \
                <img src=3D\"https://cdn.example.com/missing.png\" width=3D10 height=3D10></p></body></html>";
    format!(
        "From: <Saved by Blink>\n\
         Snapshot-Content-Location: https://example.com/news/page.html\n\
         Subject: News\n\
         MIME-Version: 1.0\n\
         Content-Type: multipart/related;\n\
         \ttype=\"text/html\";\n\
         \tboundary=\"----MultipartBoundary--abc----\"\n\
         \n\
         \n\
         ------MultipartBoundary--abc----\n\
         Content-Type: text/html\n\
         Content-ID: <frame-1@mhtml.blink>\n\
         Content-Transfer-Encoding: quoted-printable\n\
         Content-Location: https://example.com/news/page.html\n\
         \n\
         {page}\n\
         ------MultipartBoundary--abc----\n\
         Content-Type: image/png\n\
         Content-Transfer-Encoding: base64\n\
         Content-Location: https://cdn.example.com/a.png?v=1\n\
         \n\
         {}\n\
         ------MultipartBoundary--abc----\n\
         Content-Type: image/png\n\
         Content-Transfer-Encoding: base64\n\
         Content-Location: https://example.com/news/img/b.png\n\
         \n\
         {}\n\
         ------MultipartBoundary--abc------\n",
        b64(&png(6, 3)),
        b64(&png(10, 5))
    )
    .into_bytes()
}

fn import_bytes(bytes: &[u8]) -> (HtmlImport, Vec<String>) {
    let mut asked = Vec::new();
    let mut resolve = |src: &str, _: usize| {
        asked.push(src.to_string());
        None
    };
    let got = import_html_within(bytes, &Budget::standard(), Some(&mut resolve)).unwrap();
    (got, asked)
}

#[test]
fn a_word_single_file_web_page_imports_its_picture_from_its_part_1898() {
    let (got, asked) = import_bytes(&word_mht());
    // Its part's charset (windows-1252) over the page's meta.
    assert_eq!(paragraph_texts(&got.document), ["Café Gone"]);
    // The picture from its part; the src no part names goes to the
    // resolver resolved against the root's location.
    assert_eq!(asked, ["file:///C:/X/doc_files/gone.png"]);
    assert_eq!(got.media.len(), 1);
    assert_eq!(got.media[0].bytes, png(8, 4));
    let pics = pictures(&got.document.body);
    assert_eq!(inline_extent(pics[0]), Some((40 * 9525, 20 * 9525)));
    // Without images, the page alone (an image is nothing then).
    let d = import_html(&word_mht()).unwrap();
    assert_eq!(paragraph_texts(&d), ["Café"]);
}

#[test]
fn a_blink_mhtml_matches_absolute_and_relative_srcs_to_their_parts_1898() {
    let (got, asked) = import_bytes(&blink_mhtml());
    assert_eq!(paragraph_texts(&got.document)[0].trim_end(), "Hello");
    // Only the src no part holds reaches the resolver, absolute.
    assert_eq!(asked, ["https://cdn.example.com/missing.png"]);
    let bytes: Vec<&[u8]> = got.media.iter().map(|m| m.bytes.as_slice()).collect();
    assert_eq!(bytes[..2], [png(6, 3).as_slice(), png(10, 5).as_slice()]);
    // Two pictures and the placeholder.
    assert_eq!(pictures(&got.document.body).len(), 3);
}

#[test]
fn a_cid_src_names_a_part_by_its_content_id_and_start_names_the_root_1898() {
    let msg = format!(
        "Content-Type: multipart/related; boundary=b; start=\"<Root@X>\"\n\n\
         --b\n\
         Content-Type: text/html\n\
         Content-ID: <decoy@x>\n\n\
         <p>decoy</p>\n\
         --b\n\
         Content-Type: text/html; charset=utf-8\n\
         Content-ID: <root@x>\n\n\
         <p>root <img src=\"cid:Logo%40X\"></p>\n\
         --b\n\
         Content-Type: image/png\n\
         Content-ID: <logo@x>\n\
         Content-Transfer-Encoding: base64\n\n\
         {}\n\
         --b--\n",
        b64(&png(2, 2))
    );
    let (got, asked) = import_bytes(msg.as_bytes());
    assert_eq!(paragraph_texts(&got.document)[0].trim_end(), "root");
    assert!(asked.is_empty(), "{asked:?}");
    assert_eq!(got.media[0].bytes, png(2, 2));
}

#[test]
fn a_malformed_message_is_read_as_a_page_1898() {
    // No boundary: not MHTML, never an error of its own.
    let d = import_html(b"Content-Type: multipart/related\n\n<p>body</p>").unwrap();
    assert_eq!(paragraph_texts(&d).last().map(String::as_str), Some("body"));
}

#[test]
fn sniff_takes_mhtml_by_its_headers_and_its_extension_1898() {
    use crate::import::{Format, sniff};
    assert_eq!(sniff(&word_mht(), "bin"), Format::Html);
    assert_eq!(sniff(&blink_mhtml(), ""), Format::Html);
    assert_eq!(sniff(b"plain", "mht"), Format::Html);
    assert_eq!(sniff(b"plain", "MHTML"), Format::Html);
    // A mail that is not multipart/related, or a text with a colon, is not.
    assert_eq!(
        sniff(b"Subject: x\nContent-Type: text/plain\n\nhi", ""),
        Format::Unknown
    );
    assert_eq!(sniff(b"Note: x\nmore", ""), Format::Unknown);
}

const SVG: &str = "<?xml version=\"1.0\"?>\n<!-- logo -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x\" [<!ENTITY a \"b\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"30\" height=\"10\"><rect width=\"30\" height=\"10\"/></svg>";
const WEBP: &[u8] = b"RIFF\x1a\x00\x00\x00WEBPVP8L\x0d\x00\x00\x00\x2f\x00\x00\x00\x10\x07\x10\x11\x11\x88\x88\xfe\x07\x00";

fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", b64(bytes).replace("\r\n", ""))
}

/// `html` imported with a resolver that refuses everything and a converter
/// making a 4 x 2 PNG of an SVG (CSS size 30 x 10) and a 6 x 3 PNG of a
/// WebP (CSS size its pixels): the import and what it was asked to convert.
fn converting(html: &str) -> (HtmlImport, Vec<ConvertKind>) {
    let mut kinds = Vec::new();
    let mut resolve = |_: &str, _: usize| None;
    let mut convert = |kind: ConvertKind, _: &[u8], _: usize| {
        kinds.push(kind);
        Some(match kind {
            ConvertKind::Svg => html_image::ConvertedImage {
                png: png(4, 2),
                width: 30,
                height: 10,
            },
            ConvertKind::Webp => html_image::ConvertedImage {
                png: png(6, 3),
                width: 6,
                height: 3,
            },
        })
    };
    let got = import_html_with_converter(html.as_bytes(), &mut resolve, &mut convert).unwrap();
    (got, kinds)
}

use html_image::ConvertKind;

#[test]
fn without_a_converter_svg_and_webp_stay_placeholders_1898() {
    let page = format!(
        "<p><img src='{}' width=30 height=10 alt=Logo><img src='{}' alt=Pic></p>",
        data_uri("image/svg+xml", SVG.as_bytes()),
        data_uri("image/webp", WEBP)
    );
    let (got, _) = import(&page);
    // The sized one a placeholder, the unsized one its alt text.
    assert_eq!(got.media.len(), 1);
    assert_eq!(got.media[0].bytes, html_image::PLACEHOLDER_PNG);
    assert_eq!(paragraph_texts(&got.document)[0].trim(), "Pic");
    // Every medium sniffs.
    assert!(
        got.media
            .iter()
            .all(|m| crate::image::sniff(&m.bytes).is_some())
    );
}

#[test]
fn a_converted_webp_is_a_png_medium_sized_by_the_converter_1898() {
    let (got, kinds) = converting(&format!(
        "<p><img src='{}'></p>",
        data_uri("image/webp", WEBP)
    ));
    assert_eq!(kinds, [ConvertKind::Webp]);
    assert_eq!(got.media.len(), 1);
    assert_eq!(got.media[0].bytes, png(6, 3));
    let pics = pictures(&got.document.body);
    assert_eq!(inline_extent(pics[0]), Some((6 * 9525, 3 * 9525)));
    assert!(!pics[0].contains("svgBlip"));
}

#[test]
fn a_converted_svg_keeps_the_svg_beside_its_png_fallback_once_1898() {
    let uri = data_uri("image/svg+xml", SVG.as_bytes());
    let (got, kinds) = converting(&format!(
        "<p><img src='{uri}'><img src='{uri}' width=60></p>"
    ));
    // Converted once for the two pictures.
    assert_eq!(kinds, [ConvertKind::Svg]);
    assert_eq!(got.media.len(), 2);
    assert_eq!(got.media[0].bytes, png(4, 2));
    assert_eq!(got.media[1].bytes, SVG.as_bytes());
    assert_eq!(got.media[1].info.ext, "svg");
    assert_eq!(got.media[1].info.content_type, "image/svg+xml");
    let pics = pictures(&got.document.body);
    // The SVG's own size (30 x 10), not the PNG's pixels.
    assert_eq!(inline_extent(pics[0]), Some((30 * 9525, 10 * 9525)));
    assert_eq!(inline_extent(pics[1]), Some((60 * 9525, 20 * 9525)));
    // The blip names the PNG; Word's SVG extension names the SVG.
    for p in &pics {
        assert_eq!(
            crate::image::inline_embeds(p),
            [got.media[0].rid.clone(), got.media[1].rid.clone()]
        );
        assert!(p.contains(crate::image::SVG_BLIP_EXT_URI), "{p}");
    }
}

#[test]
fn a_converted_svgs_package_holds_both_parts_and_both_rels_1898() {
    let uri = data_uri("image/svg+xml", SVG.as_bytes());
    let (got, _) = converting(&format!("<p>x<img src='{uri}'></p>"));
    let pkg = got.package();
    let saved = crate::package::save_package(&pkg);
    let pkg = crate::package::load_package(&saved).unwrap();
    let names = pkg.part_names();
    let png_part = names.iter().find(|n| n.ends_with(".png")).unwrap();
    let svg_part = names.iter().find(|n| n.ends_with(".svg")).unwrap();
    assert_eq!(pkg.part(svg_part), Some(SVG.as_bytes()));
    assert_eq!(pkg.part(png_part), Some(png(4, 2).as_slice()));
    let ct = String::from_utf8(pkg.part("[Content_Types].xml").unwrap().to_vec()).unwrap();
    assert!(ct.contains("image/svg+xml"), "{ct}");
    // The picture's two rids are the package's relationships to them.
    let pics = pictures(&pkg.document.body);
    let rids = crate::image::inline_embeds(pics[0]);
    let rels =
        String::from_utf8(pkg.part("word/_rels/document.xml.rels").unwrap().to_vec()).unwrap();
    for (rid, part) in rids.iter().zip([png_part, svg_part]) {
        let target = part.trim_start_matches("word/");
        assert!(
            rels.contains(&format!("Id=\"{rid}\"")) && rels.contains(target),
            "{rid} {target}: {rels}"
        );
    }
}

#[test]
fn a_converters_bad_answer_leaves_a_placeholder_1898() {
    let uri = data_uri("image/svg+xml", SVG.as_bytes());
    let page = format!("<p><img src='{uri}' width=5 height=5></p>");
    for answer in [
        // Not a PNG.
        Some(html_image::ConvertedImage {
            png: b"GIF89a\x01\x00\x01\x00".to_vec(),
            width: 1,
            height: 1,
        }),
        // No size.
        Some(html_image::ConvertedImage {
            png: png(1, 1),
            width: 0,
            height: 1,
        }),
        None,
    ] {
        let mut resolve = |_: &str, _: usize| None;
        let mut convert = |_: ConvertKind, _: &[u8], _: usize| answer.clone();
        let got = import_html_with_converter(page.as_bytes(), &mut resolve, &mut convert).unwrap();
        assert_eq!(got.media.len(), 1);
        assert_eq!(got.media[0].bytes, html_image::PLACEHOLDER_PNG);
    }
    // One longer than the limit it was told.
    let mut resolve = |_: &str, _: usize| None;
    let mut convert = |_: ConvertKind, _: &[u8], limit: usize| {
        let mut png = png(1, 1);
        png.resize(limit + 1, 0);
        Some(html_image::ConvertedImage {
            png,
            width: 1,
            height: 1,
        })
    };
    let got = import_html_with_converter(page.as_bytes(), &mut resolve, &mut convert).unwrap();
    assert_eq!(got.media[0].bytes, html_image::PLACEHOLDER_PNG);
}

#[test]
fn a_refused_first_base_href_still_ends_the_search_r1_1898() {
    // A browser takes only the first `<base>` with an `href`, usable or not.
    for first in ["javascript:x", "data:text/html,x", ""] {
        let (_, asked) = import(&format!(
            "<base href='{first}'><base href='img/'><p><img src=a.png></p>"
        ));
        assert_eq!(asked, ["a.png"], "{first}");
    }
    // A `<base>` without an `href` does not.
    let (_, asked) = import("<base target=_top><base href='img/'><p><img src=a.png></p>");
    assert_eq!(asked, ["img/a.png"]);
}

#[test]
fn many_base_tags_that_never_close_scan_in_linear_time_r1_1898() {
    // Each `<base` that never closes (or closes only at the very end) once
    // scanned to the end: quadratic, minutes for a megabyte here (200 KB
    // took ~15 s a call in a debug build). Read once each, the pre-pass is
    // linear: milliseconds.
    for unit in ["<base ", "<base x=\"", "<base<"] {
        let mut page = String::from("<p>x</p>");
        page.push_str(&unit.repeat(1_000_000 / unit.len()));
        let start = std::time::Instant::now();
        let (css, base) = collect_styles(&page);
        assert!(
            start.elapsed().as_secs() < 20,
            "{unit}: {:?}",
            start.elapsed()
        );
        assert!(css.is_empty() && base.is_none(), "{unit}");
    }
    let mut page = "<base ".repeat(200_000);
    page.push_str("href='img/'><p><img src=a.png></p>");
    let start = std::time::Instant::now();
    let (_, base) = collect_styles(&page);
    assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    // One tag, the first `<base` to its `>`: its `href` is the base.
    assert_eq!(base.as_deref(), Some("img/"));
}

#[test]
fn candidates_stop_at_the_cap_in_a_list_and_across_a_pictures_sources_r2_1898() {
    let list: Vec<String> = (0..100)
        .map(|i| format!("m{i}-missing.png {}x", i + 2))
        .collect();
    assert_eq!(
        html_image::srcset_candidates(&list.join(", ")).len(),
        html_image::MAX_CANDIDATES
    );
    // Twenty sources of one candidate each, and an own list of ten: the
    // own ten first, then six of the sources', then nothing more is asked.
    let own: Vec<String> = (0..10).map(|i| format!("own{i}-missing.png")).collect();
    let mut page = String::from("<p><picture>");
    for i in 0..20 {
        page.push_str(&format!("<source srcset='s{i}-missing.png'>"));
    }
    page.push_str(&format!(
        "<img src=missing.png srcset='{}'></picture></p>",
        own.join(", ")
    ));
    let (_, asked) = import(&page);
    let mut want = vec!["missing.png".to_string()];
    want.extend(own.iter().cloned());
    want.extend((0..6).map(|i| format!("s{i}-missing.png")));
    assert_eq!(asked, want);
}
