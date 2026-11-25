//! The centring leftover of #1922 (#1930): a fixed-width block with both
//! side margins `auto` inside a table cell (or a card) is centred in that
//! cell's inner width, resolved when the table is laid out.

use super::*;
use crate::model::{Block as DocBlock, Cell, Paragraph, Table};

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

/// The paragraph whose text is `text`, with the cell and table it is in
/// (the innermost), `None` for a body paragraph.
fn find<'d>(d: &'d Document, text: &str) -> (&'d Paragraph, Option<(&'d Table, &'d Cell)>) {
    fn walk<'d>(
        blocks: &'d [DocBlock],
        at: Option<(&'d Table, &'d Cell)>,
        text: &str,
    ) -> Option<(&'d Paragraph, Option<(&'d Table, &'d Cell)>)> {
        for b in blocks {
            match b {
                DocBlock::Paragraph(p) if p.plain_text() == text => return Some((p, at)),
                DocBlock::Table(t) => {
                    for c in t.rows.iter().flat_map(|r| &r.cells) {
                        if let Some(hit) = walk(&c.blocks, Some((t, c)), text) {
                            return Some(hit);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&d.body, None, text).unwrap_or_else(|| panic!("no paragraph {text:?}"))
}

/// `<name w:w="N"` in `xml`, inside `<outer>…</outer>`.
fn twips_in(xml: &str, outer: &str, name: &str) -> Option<i64> {
    let open = format!("<{outer}>");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{outer}>"))?;
    let inner = &xml[start..end];
    let pat = format!("<{name} w:w=\"");
    let at = inner.find(&pat)? + pat.len();
    inner[at..].split('"').next()?.parse().ok()
}

/// The cell's inner width in twips: its `tcW` less its left and right
/// margins (its own `tcMar`, else the table's `tblCellMar`), as the layout
/// gives a nested table.
fn inner_width(t: &Table, c: &Cell) -> i64 {
    let tcpr = c.raw_tcpr.as_deref().unwrap_or_default();
    let tblpr = t.raw_tblpr.as_deref().unwrap_or_default();
    let w: i64 = tcpr
        .split("<w:tcW w:w=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.parse().ok())
        .expect("a tcW");
    let side = |name: &str| {
        twips_in(tcpr, "w:tcMar", name)
            .or_else(|| twips_in(tblpr, "w:tblCellMar", name))
            .unwrap_or(0)
    };
    w - side("w:left") - side("w:right")
}

fn indents(p: &Paragraph) -> (i32, i32) {
    (p.props.indent, p.props.indent_right)
}

/// `body` inside a one-cell table `px` wide with no cell padding.
fn in_cell(px: u32, body: &str) -> String {
    format!("<table style='width:{px}px' cellpadding='0'><tr><td>{body}</td></tr></table>")
}

#[test]
fn a_fixed_width_block_in_a_cell_is_centred_in_it() {
    let d = doc(&in_cell(
        600,
        "<div style='width:300px;margin:0 auto'>text</div><p>after</p>",
    ));
    let (p, at) = find(&d, "text");
    let (t, c) = at.expect("in a cell");
    let inner = inner_width(t, c);
    assert_eq!(inner, 9000);
    let extra = ((inner - 4500) / 2) as i32;
    assert_eq!(indents(p), (extra, extra));
    assert_eq!(extra, 2250);
    // The paragraph after it, outside the box, is not moved.
    assert_eq!(indents(find(&d, "after").0), (0, 0));
}

#[test]
fn a_centred_block_in_a_padded_cell_uses_its_inner_width() {
    for (html, pad) in [
        (
            "<table style='width:600px' cellpadding='10'><tr><td>\
             <div style='width:300px;margin:0 auto'>text</div></td></tr></table>",
            150,
        ),
        (
            "<table style='width:600px' cellpadding='0'><tr><td style='padding:0 40px'>\
             <div style='width:300px;margin:0 auto'>text</div></td></tr></table>",
            600,
        ),
    ] {
        let d = doc(html);
        let (p, at) = find(&d, "text");
        let (t, c) = at.expect("in a cell");
        let inner = inner_width(t, c);
        assert_eq!(inner, 9000 - 2 * pad, "{html}");
        let extra = ((inner - 4500) / 2) as i32;
        assert_eq!(indents(p), (extra, extra), "{html}");
    }
}

#[test]
fn a_block_wider_than_its_cell_stays_at_its_left() {
    let d = doc(&in_cell(
        300,
        "<div style='width:600px;margin:0 auto'>text</div>",
    ));
    assert_eq!(indents(find(&d, "text").0), (0, 0));
    // Its own padding is still there, never less.
    let d = doc(&in_cell(
        300,
        "<div style='width:600px;padding:0 10px;margin:0 auto'>text</div>",
    ));
    assert_eq!(indents(find(&d, "text").0), (150, 150));
}

#[test]
fn nested_centred_blocks_in_a_cell_centre_in_each_other() {
    let d = doc(&in_cell(
        600,
        "<div style='width:400px;margin:0 auto'>outer\
         <div style='width:200px;margin:0 auto'>inner</div></div>",
    ));
    // Outer: (9000 - 6000) / 2 = 1500 each side; the inner one then has
    // 9000 - 1500 - 1500 - 3000 = 3000 left over, 1500 more each side.
    assert_eq!(indents(find(&d, "outer").0), (1500, 1500));
    assert_eq!(indents(find(&d, "inner").0), (3000, 3000));
}

#[test]
fn a_centred_block_in_a_cell_keeps_its_own_padding_outside_its_width() {
    let d = doc(&in_cell(
        600,
        "<div style='width:300px;padding:0 20px;margin:0 auto'>text</div>",
    ));
    // 9000 - 300 - 300 - 4500 = 3900 over two, past the 300 padding.
    assert_eq!(indents(find(&d, "text").0), (300 + 1950, 300 + 1950));
}

/// A cell exactly as wide as the body ([`html_table::BODY_TWIPS`]) with
/// no padding: what is in it centres as the same markup does at body
/// level, lines and all.
fn same_as_body(body: &str, text: &str) {
    let px = html_table::BODY_TWIPS / 15;
    assert_eq!(px * 15, html_table::BODY_TWIPS);
    let top = doc(body);
    let cell = doc(&in_cell(px, body));
    let (want, at) = find(&top, text);
    assert!(at.is_none());
    let (got, at) = find(&cell, text);
    let (t, c) = at.expect("in a cell");
    assert_eq!(inner_width(t, c), i64::from(html_table::BODY_TWIPS));
    assert_eq!(indents(got), indents(want), "{body}");
    assert_eq!(got.props.borders.left, want.props.borders.left, "{body}");
    assert_eq!(got.props.borders.right, want.props.borders.right, "{body}");
    assert_eq!(got.props.first_line, want.props.first_line, "{body}");
}

#[test]
fn an_outer_blocks_line_stays_put_beside_a_centred_box_in_a_cell() {
    // The outer div's line is at the cell's left: the centred paragraph's
    // gap to it grows (up to Word's 31 pt), as at body level.
    let html = "<div style='border-left:2px solid red;border-right:2px solid red'>\
                <div style='width:700px;margin:0 auto'>x</div>y</div>";
    same_as_body(html, "x");
    same_as_body(html, "y");
    let d = doc(&in_cell(786, html));
    let x = find(&d, "x").0.props.borders.left.expect("the outer line");
    let y = find(&d, "y").0.props.borders.left.expect("the outer line");
    assert!(x.space > y.space, "{x:?} {y:?}");
    // A line between two centred boxes moves with the outer one only.
    same_as_body(
        "<div style='width:700px;margin:0 auto'>\
         <div style='border-left:2px solid red;padding-left:5px'>\
         <div style='width:690px;margin:0 auto'>x</div></div></div>",
        "x",
    );
}

#[test]
fn a_centred_boxs_own_line_keeps_its_space_in_a_cell() {
    let html = "<div style='width:300px;margin:0 auto;border-left:2px solid red;\
                padding-left:10px;border-right:1px solid blue'>x\
                <div style='padding-left:20px'>inner</div></div>";
    same_as_body(html, "x");
    same_as_body(html, "inner");
    // Unmoved by the centring: the gap to its own line is its padding.
    let d = doc(&in_cell(600, html));
    let (p, _) = find(&d, "x");
    assert!(p.props.indent > 0);
    let at_left = doc(&in_cell(
        600,
        "<div style='width:300px;border-left:2px solid red;padding-left:10px;\
         border-right:1px solid blue'>x</div>",
    ));
    assert_eq!(
        p.props.borders.left,
        find(&at_left, "x").0.props.borders.left
    );
}

#[test]
fn centred_blocks_in_a_body_wide_cell_match_the_body() {
    for (html, text) in [
        ("<div style='width:300px;margin:0 auto'>x</div>", "x"),
        (
            "<div style='margin-left:1in'><p style='width:2in;margin:0 auto'>x</p></div>",
            "x",
        ),
        (
            "<div style='width:400px;margin:0 auto'><div style='width:200px;\
             padding:0 7px;margin:0 auto'>x</div></div>",
            "x",
        ),
        ("<div style='max-width:300px;margin:0 auto'>x</div>", "x"),
    ] {
        same_as_body(html, text);
    }
}

#[test]
fn a_centred_block_in_a_card_is_centred_in_the_card() {
    // A card (#1927), a shaded `div` in a cell, is a one-cell table: the
    // box centres in its cell, not in the cell around the card.
    let d = doc(&in_cell(
        600,
        "<div style='background:#eeeeee;width:400px;padding:0 10px'>\
         <div style='width:200px;margin:0 auto'>text</div></div>",
    ));
    let (p, at) = find(&d, "text");
    let (t, c) = at.expect("in the card's cell");
    assert!(
        t.raw_tblpr
            .as_deref()
            .unwrap_or_default()
            .contains("w:w=\"6000\"")
    );
    // Its padding is its cell's margins (150 each side), outside the room.
    let inner = inner_width(t, c);
    assert_eq!(inner, 6000 - 2 * 150);
    let extra = ((inner - 3000) / 2) as i32;
    assert_eq!(indents(p), (extra, extra));
}

#[test]
fn a_centred_block_in_a_cell_keeps_its_indent_in_the_saved_xml() {
    let d = doc(&in_cell(
        600,
        "<div style='width:300px;margin:0 auto'>text</div>",
    ));
    let xml = crate::serialize::document_to_xml(&d);
    assert!(
        xml.contains(r#"<w:ind w:left="2250" w:right="2250"/>"#),
        "{xml}"
    );
    // A list item at the box's edge: its explicit zero indent goes, the
    // moved indent is written instead.
    let list = "<div style='width:300px;margin:0 auto'>\
                <ul style='padding-left:0;margin:0'><li>item</li></ul></div>";
    let d = doc(&in_cell(600, list));
    let (p, _) = find(&d, "item");
    assert_eq!(p.props.indent, 2250);
    assert!(p.props.num_id.is_some());
    // The left indent is direct, so nothing pins it to 0 (#2231); only the
    // first line, which the hang clip against the indent before centring
    // left at 0 (a known limit), stays pinned.
    assert!(
        !p.props.shadow.iter().any(|s| s.contains("w:left")),
        "{p:?}"
    );
    let xml = crate::serialize::document_to_xml(&d);
    assert!(!xml.contains(ZERO_IND), "{xml}");
}

#[test]
fn a_centred_block_is_centred_only_in_a_cell_whose_width_the_page_declares() {
    let boxed = "<div style='width:100px;margin:0 auto'>x</div>";
    let x = |html: &str| indents(find(&doc(html), "x").0);
    // An auto table: a browser shrinks it to the box, at the left.
    assert_eq!(
        x(&format!(
            "<table><tr><td>{boxed}</td><td>y</td></tr></table>"
        )),
        (0, 0)
    );
    // The cell's own width, or a `<col>`'s: 300 px = 4500, (4500 - 1500) / 2.
    assert_eq!(
        x(&format!(
            "<table cellpadding=0><tr><td style='width:300px'>{boxed}</td><td>y</td></tr></table>"
        )),
        (1500, 1500)
    );
    assert_eq!(
        x(&format!(
            "<table cellpadding=0><col width=300><col><tr><td>{boxed}</td><td>y</td></tr></table>"
        )),
        (1500, 1500)
    );
    // A percentage of an auto table's width is not declared either.
    assert_eq!(
        x(&format!(
            "<table><tr><td style='width:50%'>{boxed}</td><td>y</td></tr></table>"
        )),
        (0, 0)
    );
    // Nested: an auto table in a declared cell is not; a percentage one
    // is, of its declared cell ((9000 - 1500) / 2).
    let nested = |inner: &str| in_cell(600, &format!("{inner}<tr><td>{boxed}</td></tr></table>"));
    assert_eq!(x(&nested("<table>")), (0, 0));
    assert_eq!(x(&nested("<table width=100% cellpadding=0>")), (3750, 3750));
    // And a percentage one in an auto table's cell is not.
    assert_eq!(
        x(&format!(
            "<table><tr><td><table width=100%><tr><td>{boxed}</td></tr></table></td></tr></table>"
        )),
        (0, 0)
    );
}
