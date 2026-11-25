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
fn resolve_ref_follows_rfc_3986_1898() {
    use html_image::resolve_ref as r;
    let base = "http://a/b/c/d;p?q";
    // RFC 3986 section 5.4.1's normal examples.
    for (reference, want) in [
        ("g:h", "g:h"),
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g"),
        ("?y", "http://a/b/c/d;p?y"),
        ("g?y", "http://a/b/c/g?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("g#s", "http://a/b/c/g#s"),
        ("g?y#s", "http://a/b/c/g?y#s"),
        (";x", "http://a/b/c/;x"),
        ("g;x", "http://a/b/c/g;x"),
        (".", "http://a/b/c/"),
        ("./", "http://a/b/c/"),
        ("..", "http://a/b/"),
        ("../", "http://a/b/"),
        ("../g", "http://a/b/g"),
        ("../..", "http://a/"),
        ("../../g", "http://a/g"),
        // Abnormal: no `..` above the root.
        ("../../../g", "http://a/g"),
        ("/./g", "http://a/g"),
        ("g/./h", "http://a/b/c/g/h"),
        ("g/../h", "http://a/b/c/h"),
    ] {
        assert_eq!(r(base, reference), want, "{reference}");
    }
    // A host with no path.
    assert_eq!(
        r("https://example.com", "a.png"),
        "https://example.com/a.png"
    );
    // A `file:` base, as Word's MHTML root.
    assert_eq!(
        r("file:///C:/X/doc.htm", "doc_files/image001.png"),
        "file:///C:/X/doc_files/image001.png"
    );
    // A relative base gives a relative src; a `..` it cannot remove stays.
    assert_eq!(r("img/", "a.png"), "img/a.png");
    assert_eq!(r("img/x/", "../a.png"), "img/a.png");
    assert_eq!(r("../shared/", "a.png"), "../shared/a.png");
    // A src with a scheme is itself.
    assert_eq!(r("img/", "cid:logo@x"), "cid:logo@x");
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
fn srcset_candidates_parse_as_the_html_standard_splits_them_1898() {
    use html_image::srcset_candidates as c;
    let own = |v: &[(&str, f64)]| -> Vec<(String, f64)> {
        v.iter().map(|(u, d)| (u.to_string(), *d)).collect()
    };
    // `1x` and bare first, then the rest as written; `w` is 1.
    assert_eq!(
        c("big.png 2x, small.png 1x, plain.png, wide.png 800w"),
        own(&[
            ("small.png", 1.0),
            ("plain.png", 1.0),
            ("big.png", 2.0),
            ("wide.png", 1.0)
        ])
    );
    // A `data:` URI keeps its commas; a trailing comma ends a candidate.
    assert_eq!(
        c("data:image/png;base64,AA,BB 2x,a.png, b.png 1.5x"),
        own(&[
            ("a.png", 1.0),
            ("data:image/png;base64,AA,BB", 2.0),
            ("b.png", 1.5)
        ])
    );
    // A URL runs to white space, commas and all, as the standard reads it.
    assert_eq!(c("a.png,b.png 2x"), own(&[("a.png,b.png", 2.0)]));
    // Bad descriptors drop the candidate: two, zero, unknown, negative.
    assert_eq!(
        c("a.png 1x 2x, b.png 0x, c.png 3q, d.png -1x, e.png 2X"),
        own(&[("e.png", 2.0)])
    );
    // Parentheses hold a comma inside descriptors (a future descriptor).
    assert_eq!(c("a.png foo(1,2), b.png"), own(&[("b.png", 1.0)]));
    assert!(c("  , ,").is_empty());
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
