//! The leftovers of #1927 (#1940): a background `div` in the body that is
//! a box of its own is a card (a one-cell table) as one in a cell is, laid
//! out in the room the blocks around it leave; two tables back to back in
//! the body are kept apart by an empty paragraph.

use super::tests_1898::png;
use super::*;
use crate::image::inline_extent;
use crate::model::{Block as DocBlock, Paragraph, Table};

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

fn tables(blocks: &[DocBlock]) -> Vec<&Table> {
    blocks
        .iter()
        .filter_map(|b| match b {
            DocBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// The body's only table.
fn only_table(d: &Document) -> &Table {
    let t = tables(&d.body);
    assert_eq!(t.len(), 1, "{:?}", d.body);
    t[0]
}

fn tcpr(t: &Table) -> &str {
    t.rows[0].cells[0].raw_tcpr.as_deref().unwrap_or("")
}

fn tblpr(t: &Table) -> &str {
    t.raw_tblpr.as_deref().unwrap_or("")
}

/// Every table, nested ones too.
fn table_count(blocks: &[DocBlock]) -> usize {
    tables(blocks)
        .iter()
        .map(|t| {
            1 + t
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .map(|c| table_count(&c.blocks))
                .sum::<usize>()
        })
        .sum()
}

fn all_paras(blocks: &[DocBlock]) -> Vec<&Paragraph> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            DocBlock::Paragraph(p) => out.push(p),
            DocBlock::Table(t) => {
                for c in t.rows.iter().flat_map(|r| &r.cells) {
                    out.extend(all_paras(&c.blocks));
                }
            }
            _ => {}
        }
    }
    out
}

fn para<'d>(d: &'d Document, text: &str) -> &'d Paragraph {
    all_paras(&d.body)
        .into_iter()
        .find(|p| p.plain_text() == text)
        .unwrap_or_else(|| panic!("no paragraph {text:?}: {:?}", d.body))
}

fn shaded(p: &Paragraph) -> bool {
    p.props.raw_props.iter().any(|r| r.starts_with("<w:shd"))
}

/// The body's paragraphs and tables as a shape: `P` an empty paragraph,
/// `P(text)` another, `T` a table.
fn shape(d: &Document) -> Vec<String> {
    d.body
        .iter()
        .filter_map(|b| match b {
            DocBlock::Paragraph(p) if p.plain_text().is_empty() => Some("P".to_string()),
            DocBlock::Paragraph(p) => Some(format!("P({})", p.plain_text())),
            DocBlock::Table(_) => Some("T".to_string()),
            _ => None,
        })
        .collect()
}

/// `<w:tcMar>` with these sides, in twips.
fn tcmar(top: u32, left: u32, bottom: u32, right: u32) -> String {
    format!(
        "<w:tcMar><w:top w:w=\"{top}\" w:type=\"dxa\"/><w:left w:w=\"{left}\" w:type=\"dxa\"/>\
         <w:bottom w:w=\"{bottom}\" w:type=\"dxa\"/><w:right w:w=\"{right}\" w:type=\"dxa\"/></w:tcMar>"
    )
}

const BODY: u32 = html_table::BODY_TWIPS;

#[test]
fn a_padded_background_div_in_the_body_is_a_card_1940() {
    let d = doc("<p>before</p><div style='background:#eee;padding:8px'>x</div><p>after</p>");
    assert_eq!(shape(&d), ["P(before)", "T", "P(after)"]);
    let t = only_table(&d);
    assert_eq!((t.rows.len(), t.rows[0].cells.len()), (1, 1));
    let pr = tcpr(t);
    assert!(pr.contains(r#"w:fill="EEEEEE""#), "{pr}");
    assert!(pr.contains(&tcmar(120, 120, 120, 120)), "{pr}");
    assert!(pr.contains(r#"<w:vAlign w:val="top"/>"#), "{pr}");
    let tp = tblpr(t);
    assert!(tp.contains(r#"<w:tblW w:w="5000" w:type="pct"/>"#), "{tp}");
    assert!(!tp.contains("<w:jc") && !tp.contains("tblInd"), "{tp}");
    let x = para(&d, "x");
    assert!(!shaded(x), "{:?}", x.props.raw_props);
    assert_eq!((x.props.indent, x.props.indent_right), (0, 0));
    // A width or a `max-width` alone makes it a box too.
    for style in ["width:50%", "max-width:600px"] {
        let d = doc(&format!("<div style='background:#eee;{style}'>x</div>"));
        assert_eq!(table_count(&d.body), 1, "{style}");
        assert!(!shaded(para(&d, "x")), "{style}");
    }
}

#[test]
fn a_table_in_a_body_card_is_on_its_fill_1940() {
    let d = doc("<div style='background:#eee;padding:8px'><p>head</p>\
         <table><tr><td>cell</td></tr></table></div>");
    let card = only_table(&d);
    assert_eq!(table_count(&d.body), 2);
    let inner = &card.rows[0].cells[0].blocks;
    assert_eq!(tables(inner).len(), 1, "{inner:?}");
    assert!(!shaded(para(&d, "cell")));
    // A card in a body card is nested in its cell (#1927's path).
    let d = doc("<div style='background:#ddd;padding:4px'>\
         <div style='background:#eee;padding:8px'>x</div></div>");
    assert_eq!(table_count(&d.body), 2);
    let inner = tables(&only_table(&d).rows[0].cells[0].blocks)[0];
    assert!(tcpr(inner).contains(r#"w:fill="EEEEEE""#));
}

#[test]
fn a_body_card_takes_its_width_place_and_max_width_1940() {
    let d = doc("<div style='background:#eee;width:400px;margin:0 auto'>x</div>");
    let tp = tblpr(only_table(&d));
    assert!(tp.contains(r#"<w:tblW w:w="6000" w:type="dxa"/>"#), "{tp}");
    assert!(tp.contains(r#"<w:jc w:val="center"/>"#), "{tp}");
    // `max-width` (absolute) bounds the full width, and auto margins
    // centre what is left.
    let d = doc("<div style='background:#eee;max-width:600px;margin:0 auto'>x</div>");
    let tp = tblpr(only_table(&d));
    assert!(tp.contains(r#"<w:tblW w:w="9000" w:type="dxa"/>"#), "{tp}");
    assert!(tp.contains(r#"<w:jc w:val="center"/>"#), "{tp}");
    // A smaller width stays; a larger one is cut to it.
    let d = doc("<div style='background:#eee;width:300px;max-width:600px'>x</div>");
    assert!(tblpr(only_table(&d)).contains(r#"<w:tblW w:w="4500" w:type="dxa"/>"#));
    let d = doc("<div style='background:#eee;width:700px;max-width:600px'>x</div>");
    assert!(tblpr(only_table(&d)).contains(r#"<w:tblW w:w="9000" w:type="dxa"/>"#));
    // A percentage `max-width` is none (as for a block, #1894).
    let d = doc("<div style='background:#eee;padding:1px;max-width:50%'>x</div>");
    assert!(tblpr(only_table(&d)).contains(r#"<w:tblW w:w="5000" w:type="pct"/>"#));
    // In a cell too: the card is no wider than it.
    let d = doc(
        "<table width=700><tr><td><div style='background:#eee;max-width:300px'>x</div>\
         </td></tr></table>",
    );
    let card = tables(&only_table(&d).rows[0].cells[0].blocks)[0];
    assert!(card.grid.iter().sum::<u32>() <= 4500, "{:?}", card.grid);
}

#[test]
fn a_body_card_is_laid_out_in_the_room_around_it_1940() {
    // A blockquote's 40 px margins: the room between them, centred, so
    // the card fills it exactly.
    let d = doc("<blockquote><div style='background:#eee;padding:8px'>x</div></blockquote>");
    let tp = tblpr(only_table(&d));
    let room = BODY - 2 * 600;
    assert!(
        tp.contains(&format!(r#"<w:tblW w:w="{room}" w:type="dxa"/>"#)),
        "{tp}"
    );
    assert!(tp.contains(r#"<w:jc w:val="center"/>"#), "{tp}");
    // Its own side margins too.
    let d = doc("<div style='background:#eee;padding:8px;margin:0 24px'>x</div>");
    let tp = tblpr(only_table(&d));
    let room = BODY - 2 * 360;
    assert!(
        tp.contains(&format!(r#"<w:tblW w:w="{room}" w:type="dxa"/>"#)),
        "{tp}"
    );
    // One side only: no longer centred, at its indent.
    let d = doc("<div style='background:#eee;padding:8px;margin-left:24px'>x</div>");
    let tp = tblpr(only_table(&d));
    let room = BODY - 360;
    assert!(
        tp.contains(&format!(r#"<w:tblW w:w="{room}" w:type="dxa"/>"#)),
        "{tp}"
    );
    assert!(tp.contains(r#"<w:tblInd w:w="360" w:type="dxa"/>"#), "{tp}");
    assert!(!tp.contains("<w:jc"), "{tp}");
    // In a centred `max-width` wrapper with no background (#2114).
    let d = doc("<div style='max-width:600px;margin:0 auto'>\
         <div style='background:#eee;padding:8px'>x</div></div>");
    let tp = tblpr(only_table(&d));
    assert!(tp.contains(r#"<w:tblW w:w="9000" w:type="dxa"/>"#), "{tp}");
    assert!(tp.contains(r#"<w:jc w:val="center"/>"#), "{tp}");
}

#[test]
fn a_percentage_picture_in_a_body_card_is_of_its_room_1940() {
    let mut resolve = |_: &str, _: usize| Some(png(40, 20));
    let got = import_html_within(
        b"<div style='background:#eee;padding:8px;margin:0 24px'>\
          <img src=a.png style='width:50%'></div>",
        &Budget::standard(),
        Some(&mut resolve),
    )
    .unwrap();
    let pics = super::tests_1898::pictures(&got.document.body);
    assert_eq!(pics.len(), 1);
    let (cx, _) = inline_extent(pics[0]).unwrap();
    // The room less its margins, less its 8 px padding each side.
    let inner = i64::from(BODY - 2 * 360 - 2 * 120);
    assert_eq!(cx / 635, inner / 2);
}

#[test]
fn what_around_it_draws_on_paragraphs_keeps_a_body_div_shading_1940() {
    let card = "<div style='background:#eee;padding:8px'>x</div>";
    for around in [
        // A list item: its number and indent.
        "<ul><li>{}</li></ul>",
        // A quoted reply's left rule.
        "<blockquote style='border-left:1px solid #ccc;padding-left:8px'>{}</blockquote>",
        // Outlook's reply header line.
        "<div style='border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in'>{}</div>",
        // A shaded block that is no card.
        "<div style='background:#ddd'>{}</div>",
    ] {
        let d = doc(&around.replace("{}", card));
        assert_eq!(table_count(&d.body), 0, "{around}");
        assert!(shaded(para(&d, "x")), "{around}");
    }
    let d = doc(&format!("<ul><li>{card}</li></ul>"));
    assert!(para(&d, "x").props.num_id.is_some());
    let d = doc(&format!(
        "<blockquote style='border-left:1px solid #ccc;padding-left:8px'>{card}</blockquote>"
    ));
    assert!(para(&d, "x").props.borders.left.is_some());
    let d = doc(&format!(
        "<div style='border-top:solid #E1E1E1 1.0pt;padding:3pt 0 0 0'>{card}</div>"
    ));
    assert!(para(&d, "x").props.borders.top.is_some());
}

#[test]
fn a_background_alone_in_the_body_stays_paragraph_shading_1940() {
    // New Outlook's body: a white `div` per line, no box of its own.
    let d = doc(
        "<div class=elementToProof style='font-family:Aptos;font-size:12pt;\
         color:rgb(0,0,0);background-color:rgb(255,255,255)'>a</div>\
         <div class=elementToProof style='background-color:rgb(255,255,255)'>b</div>",
    );
    assert_eq!(shape(&d), ["P(a)", "P(b)"]);
    assert!(shaded(para(&d, "a")) && shaded(para(&d, "b")));
}

#[test]
fn body_divs_that_are_not_cards_are_as_before_1940() {
    // Empty: an e-mail's divider makes nothing.
    let d = doc("<p>a</p><div style='background:#ccc;padding:1px'></div><p>b</p>");
    assert_eq!(shape(&d), ["P(a)", "P(b)"]);
    for div in [
        "<div style='display:none;background:#eee;padding:8px'>x</div>",
        "<div style='mso-hide:all;background:#eee;padding:8px'>x</div>",
    ] {
        let d = doc(&format!("{div}<p>y</p>"));
        assert_eq!(table_count(&d.body), 0, "{div}");
    }
    for div in [
        "<div style='display:inline-block;background:#eee;padding:8px'>x</div>",
        "<div style='background:#eee;padding:8px;background:none'>x</div>",
        "<div style='background:#eee;padding:8px;background-color:transparent'>x</div>",
    ] {
        let d = doc(div);
        assert_eq!(table_count(&d.body), 0, "{div}");
    }
    // Padding without a background is indent.
    let d = doc("<div style='padding:8px'>x</div>");
    assert_eq!(table_count(&d.body), 0);
    assert_eq!(para(&d, "x").props.indent, 120);
}

#[test]
fn tables_back_to_back_in_the_body_are_kept_apart_1940() {
    let one = "<table><tr><td>a</td></tr></table>";
    let two = "<table><tr><td>b</td></tr></table>";
    let d = doc(&format!("{one}{two}"));
    assert_eq!(shape(&d), ["T", "P", "T"]);
    // White space between is still nothing.
    let d = doc(&format!("{one}\n  {two}"));
    assert_eq!(shape(&d), ["T", "P", "T"]);
    // After a paragraph, nothing extra.
    let d = doc(&format!("<p>x</p>{one}"));
    assert_eq!(shape(&d), ["P(x)", "T"]);
    // Two stacked cards.
    let card = "<div style='background:#eee;padding:8px'>c</div>";
    let d = doc(&format!("{card}{card}"));
    assert_eq!(shape(&d), ["T", "P", "T"]);
    // A dropped empty card between two tables: one separator.
    let d = doc(&format!(
        "{one}<div style='background:#ccc;padding:1px'></div>{two}"
    ));
    assert_eq!(shape(&d), ["T", "P", "T"]);
}

#[test]
fn a_stray_table_part_close_leaves_a_body_card_open_1940() {
    // A browser ignores `</td>`, `</tr>` and `</table>` with no table open.
    for close in ["</td>", "</tr>", "</table>", "</tbody>"] {
        let d = doc(&format!(
            "<div style='background:#eee;padding:8px'><p>a</p>{close}<p>b</p></div><p>c</p>"
        ));
        assert_eq!(shape(&d), ["T", "P(c)"], "{close}");
        let card = only_table(&d);
        let texts: Vec<String> = all_paras(&card.rows[0].cells[0].blocks)
            .iter()
            .map(|p| p.plain_text())
            .collect();
        assert_eq!(texts, ["a", "b"], "{close}");
    }
}

#[test]
fn a_bordered_body_div_keeps_its_lines_as_paragraph_shading_1940() {
    // An alert's left rule, a boxed note, a top rule: the paragraphs
    // draw them (#1883); a card would drop them.
    let d = doc("<div style='background:#eee;border-left:4px solid red;padding:8px'>x</div>");
    assert_eq!(table_count(&d.body), 0);
    let x = para(&d, "x");
    assert!(shaded(x));
    assert!(x.props.borders.left.is_some());
    let d = doc("<div style='background:#eee;border:1px solid #ccc;padding:8px'>x</div>");
    assert_eq!(table_count(&d.body), 0);
    let b = &para(&d, "x").props.borders;
    assert!(b.left.is_some() && b.right.is_some() && b.top.is_some() && b.bottom.is_some());
    let d = doc("<div style='background:#eee;border-top:2px solid #333;padding:8px'>x</div>");
    assert_eq!(table_count(&d.body), 0);
    assert!(para(&d, "x").props.borders.top.is_some());
    // A border that draws nothing is no line.
    let d = doc("<div style='background:#eee;border:none;padding:8px'>x</div>");
    assert_eq!(table_count(&d.body), 1);
    // In a cell, #1927's card as before.
    let d = doc(
        "<table><tr><td><div style='background:#eee;border-left:4px solid red;padding:8px'>x\
         </div></td></tr></table>",
    );
    assert_eq!(table_count(&d.body), 2);
}

/// The cell blocks of the body's only table (a body card).
fn card_cell(d: &Document) -> &[DocBlock] {
    &only_table(d).rows[0].cells[0].blocks
}

#[test]
fn in_a_body_card_a_div_takes_the_bodys_rule_1940() {
    let wrap = |inner: &str| format!("<div style='background:#f4f4f4;padding:20px'>{inner}</div>");
    // An alert in a padded wrapper keeps its left rule: shaded, ruled
    // paragraphs in the wrapper card's cell.
    let d = doc(&wrap(
        "<div style='background:#fff3cd;border-left:4px solid #ffc107;padding:12px'>Warning</div>",
    ));
    assert_eq!(table_count(&d.body), 1);
    let w = para(&d, "Warning");
    assert!(shaded(w) && w.props.borders.left.is_some(), "{:?}", w.props);
    // New Outlook's white lines in it: two shaded paragraphs, no tables,
    // no separator.
    let d = doc(&wrap(
        "<div style='background-color:rgb(255,255,255)'>a</div>\
         <div style='background-color:rgb(255,255,255)'>b</div>",
    ));
    assert_eq!(table_count(&d.body), 1);
    let texts: Vec<String> = card_cell(&d)
        .iter()
        .map(|b| match b {
            DocBlock::Paragraph(p) => p.plain_text(),
            _ => "[table]".to_string(),
        })
        .collect();
    assert_eq!(texts, ["a", "b"]);
    assert!(shaded(para(&d, "a")) && shaded(para(&d, "b")));
    // A padded plain card in it is still a nested card.
    let d = doc(&wrap("<div style='background:#eee;padding:8px'>x</div>"));
    assert_eq!(table_count(&d.body), 2);
    assert!(!shaded(para(&d, "x")));
    // The guards over what is open in the card: a list item, a ruled
    // block, a top line.
    for around in [
        "<ul><li>{}</li></ul>",
        "<div style='border-left:1px solid #ccc;padding-left:8px'>{}</div>",
        "<div style='border-top:1px solid #ccc'>{}</div>",
    ] {
        let d = doc(&wrap(
            &around.replace("{}", "<div style='background:#eee;padding:8px'>x</div>"),
        ));
        assert_eq!(table_count(&d.body), 1, "{around}");
        assert!(shaded(para(&d, "x")), "{around}");
    }
    // In a real cell inside the body card: #1927's rule (a bordered div
    // is a card there).
    let d = doc(&wrap(
        "<table><tr><td><div style='background:#eee;border-left:4px solid red'>x</div>\
         </td></tr></table>",
    ));
    assert_eq!(table_count(&d.body), 3);
    assert!(!shaded(para(&d, "x")));
}

#[test]
fn a_stray_table_part_open_in_a_body_card_is_ignored_1940() {
    for html in [
        "<div style='background:#eee;padding:8px'>a<td>b</td>c</div>",
        "<div style='background:#eee;padding:8px'>a<th>b</th>c</div>",
        "<div style='background:#eee;padding:8px'>a<tr>b</tr>c</div>",
        "<div style='background:#eee;padding:8px'>a<tbody><tr><td>b</td></tr></tbody>c</div>",
    ] {
        let d = doc(html);
        let t = only_table(&d);
        assert_eq!((t.rows.len(), t.rows[0].cells.len()), (1, 1), "{html}");
        assert!(
            tcpr(t).contains(r#"w:fill="EEEEEE""#),
            "{html}: {}",
            tcpr(t)
        );
        let text: String = all_paras(card_cell(&d))
            .iter()
            .map(|p| p.plain_text())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(text.split_whitespace().collect::<String>(), "abc", "{html}");
    }
    // A real table in the card keeps its parts.
    let d = doc(
        "<div style='background:#eee;padding:8px'><table><tr><td>a</td><td>b</td></tr></table></div>",
    );
    let inner = tables(card_cell(&d))[0];
    assert_eq!(inner.rows[0].cells.len(), 2);
}

#[test]
fn a_heading_keeps_its_level_over_a_body_card_1940() {
    let h2 = "<h2><div style='background:#eee;padding:8px'>Title</div></h2>";
    let d = doc(h2);
    assert_eq!(table_count(&d.body), 0);
    let t = para(&d, "Title");
    assert_eq!(t.props.style_id.as_deref(), Some("Heading2"));
    assert!(shaded(t));
    // In a body card: the wrapper is the only table, the heading in it.
    let d = doc(&format!(
        "<div style='background:#f4f4f4;padding:20px'>{h2}</div>"
    ));
    assert_eq!(table_count(&d.body), 1);
    let t = para(&d, "Title");
    assert_eq!(t.props.style_id.as_deref(), Some("Heading2"));
    assert!(
        all_paras(card_cell(&d))
            .iter()
            .any(|p| p.plain_text() == "Title")
    );
}
