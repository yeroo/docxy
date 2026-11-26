//! The leftovers of #2114 (#2151), each laid out as a browser does: a
//! block's vertical box next to a child table, an empty block's margins
//! collapsing through it, a table in an indented block laid out in its
//! content box, and a padding-less card at a quirky cell edge.

use super::*;
use crate::model::{Block as DocBlock, Paragraph, Table};

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

/// The same page in standards mode.
fn standards(html: &str) -> Document {
    doc(&format!("<!DOCTYPE html>{html}"))
}

fn all_paras(d: &Document) -> Vec<Paragraph> {
    fn walk(blocks: &[DocBlock], out: &mut Vec<Paragraph>) {
        for b in blocks {
            match b {
                DocBlock::Paragraph(p) => out.push(p.clone()),
                DocBlock::Table(t) => {
                    for c in t.rows.iter().flat_map(|r| &r.cells) {
                        walk(&c.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&d.body, &mut out);
    out
}

fn para_of(d: &Document, text: &str) -> Paragraph {
    let all = all_paras(d);
    let texts: Vec<String> = all.iter().map(|p| p.plain_text()).collect();
    all.into_iter()
        .find(|p| p.plain_text() == text)
        .unwrap_or_else(|| panic!("no paragraph is {text:?} in {texts:?}"))
}

/// Before and after spacing.
fn gaps(d: &Document, text: &str) -> (Option<i32>, Option<i32>) {
    let p = para_of(d, text);
    (p.props.spacing.before, p.props.spacing.after)
}

/// The body's top-level tables.
fn tables(d: &Document) -> Vec<&Table> {
    d.body
        .iter()
        .filter_map(|b| match b {
            DocBlock::Table(t) => Some(t),
            _ => None,
        })
        .collect()
}

fn tblpr(t: &Table) -> &str {
    t.raw_tblpr.as_deref().unwrap_or("")
}

const TABLE: &str = "<table width='100%'><tr><td>t</td></tr></table>";

// Item 4: a table in an indented block.

/// A blockquote's room is centred in the body (40px each side): a table
/// filling it is centred there, as wide as the room.
#[test]
fn a_full_table_in_a_blockquote_is_centred_in_its_room() {
    let d = standards(&format!("<blockquote>{TABLE}</blockquote>"));
    let t = tables(&d)[0];
    let pr = tblpr(t);
    assert_eq!(t.grid.iter().sum::<u32>(), 11790 - 2 * 600);
    assert!(
        pr.contains("<w:tblW w:w=\"10590\" w:type=\"dxa\"/>"),
        "{pr}"
    );
    assert!(pr.contains("<w:jc w:val=\"center\"/>"), "{pr}");
    assert!(!pr.contains("w:tblInd"), "{pr}");
}

/// A narrower one starts at the room's left: an indent, the first cell's
/// left margin added (no compatibilityMode).
#[test]
fn a_half_table_in_a_blockquote_starts_at_its_room_s_left() {
    let d = standards("<blockquote><table width='50%'><tr><td>t</td></tr></table></blockquote>");
    let t = tables(&d)[0];
    let pr = tblpr(t);
    assert_eq!(t.grid.iter().sum::<u32>(), 10590 / 2);
    assert!(!pr.contains("<w:jc "), "{pr}");
    assert!(
        pr.contains(&format!("<w:tblInd w:w=\"{}\" w:type=\"dxa\"/>", 600 + 108)),
        "{pr}"
    );
}

/// A `margin-left` div and a list item: the room right of the indent.
#[test]
fn a_table_in_a_left_indented_block_is_laid_out_right_of_it() {
    for page in [
        format!("<div style='margin-left:40px'>{TABLE}</div>"),
        format!("<ul><li>{TABLE}</li></ul>"),
        format!("<ol><li>a{TABLE}</li></ol>"),
    ] {
        let d = standards(&page);
        let t = tables(&d)[0];
        let pr = tblpr(t);
        assert_eq!(t.grid.iter().sum::<u32>(), 11790 - 600, "{page}");
        assert!(
            pr.contains("<w:tblW w:w=\"11190\" w:type=\"dxa\"/>"),
            "{pr}"
        );
        assert!(!pr.contains("<w:jc "), "{pr}");
        assert!(
            pr.contains("<w:tblInd w:w=\"708\" w:type=\"dxa\"/>"),
            "{pr}"
        );
    }
}

/// A table in a cell or in an unindented block keeps the body's width and
/// no indent.
#[test]
fn a_table_in_a_cell_or_an_unindented_block_is_unchanged() {
    for page in [TABLE.to_string(), format!("<div>{TABLE}</div>")] {
        let d = standards(&page);
        let pr = tblpr(tables(&d)[0]);
        assert!(pr.contains("w:type=\"pct\""), "{pr}");
        assert!(!pr.contains("w:tblInd") && !pr.contains("<w:jc "), "{pr}");
    }
    let d = standards(&format!(
        "<table width='600'><tr><td><blockquote>{TABLE}</blockquote></td></tr></table>"
    ));
    let DocBlock::Table(inner) = &tables(&d)[0].rows[0].cells[0].blocks[0] else {
        panic!("no nested table");
    };
    assert!(tblpr(inner).contains("w:type=\"pct\""));
    assert!(!tblpr(inner).contains("w:tblInd"));
}

/// Word measures an older-mode indent to the first cell's text, so the
/// indent adds that cell's own left padding, as `table::left_edge` takes it
/// off: the table's edge lands at the room's left either way.
#[test]
fn an_indented_table_adds_its_first_cell_s_own_left_padding() {
    for (cell, ind) in [
        ("<td style='padding-left:10px'>", 600 + 150),
        ("<td>", 600 + 108),
    ] {
        let d = standards(&format!(
            "<div style='margin-left:40px'><table width='300'><tr>{cell}t</td></tr></table></div>"
        ));
        let t = tables(&d)[0];
        let pr = tblpr(t);
        assert!(
            pr.contains(&format!("<w:tblInd w:w=\"{ind}\" w:type=\"dxa\"/>")),
            "{pr}"
        );
        assert_eq!(crate::table::left_edge(t, None), 600, "{cell}");
    }
    let d = standards(
        "<div style='margin-left:40px'><table width='300' cellpadding='4'><tr><td>t</td></tr></table></div>",
    );
    let t = tables(&d)[0];
    assert!(tblpr(t).contains("<w:tblInd w:w=\"660\""), "{}", tblpr(t));
    assert_eq!(crate::table::left_edge(t, None), 600);
}

// Item 2: a block's box next to its first or last child table.

const CELL: &str = "<table><tr><td>t</td></tr></table>";

/// A block whose last child is a table: its bottom margin waits for the
/// next paragraph, collapsing with that one's own top (20px with p's 16px).
#[test]
fn a_block_ending_in_a_table_gives_its_bottom_to_the_next_paragraph() {
    let d = standards(&format!(
        "<div style='margin-bottom:20px'>a{CELL}</div><p>b</p>"
    ));
    assert_eq!(gaps(&d, "b"), (Some(300), Some(240)));
    // Nested blocks ending in the table collapse into one margin first.
    let d = standards(&format!(
        "<div style='margin-bottom:20px'><div style='margin-bottom:30px'>{CELL}</div></div><p>b</p>"
    ));
    assert_eq!(gaps(&d, "b"), (Some(450), Some(240)));
    // Its padding is paid below the table, then the margins collapse:
    // 10px + max(20px, 16px).
    let d = standards(&format!(
        "<div style='padding-bottom:10px;margin-bottom:20px'>a{CELL}</div><p>b</p>"
    ));
    assert_eq!(gaps(&d, "b"), (Some(150 + 300), Some(240)));
    // In a cell too.
    let d = standards(&format!(
        "<table><tr><td><div style='margin-bottom:20px'>a{CELL}</div><p>b</p></td></tr></table>"
    ));
    assert_eq!(gaps(&d, "b"), (Some(300), Some(240)));
}

/// A block whose first child is a table: its top margin goes on the
/// paragraph above, collapsing with that one's bottom (16px with 30px).
#[test]
fn a_block_starting_with_a_table_gives_its_top_to_the_paragraph_above() {
    let d = standards(&format!(
        "<p>a</p><div style='margin-top:30px'>{CELL}</div>"
    ));
    assert_eq!(gaps(&d, "a"), (Some(240), Some(450)));
    // Its padding is inside the margin: the margins collapse, then it is
    // paid.
    let d = standards(&format!(
        "<p>a</p><div style='margin-top:30px;padding-top:10px'>{CELL}</div>"
    ));
    assert_eq!(gaps(&d, "a"), (Some(240), Some(450 + 150)));
    // The block is stale past the table: the paragraph after it has no top
    // from it.
    let d = standards(&format!(
        "<p>a</p><div style='margin-top:30px'>{CELL}c</div>"
    ));
    assert_eq!(gaps(&d, "c").0, Some(0));
    // A paragraph in the block before the table takes the top as before.
    let d = standards(&format!(
        "<p>a</p><div style='margin-top:30px'>x{CELL}</div>"
    ));
    assert_eq!((gaps(&d, "a").1, gaps(&d, "x").0), (Some(240), Some(210)));
}

/// With no paragraph to carry it, the box is dropped as before, and a
/// card (a shaded `div`) keeps its own limit: neither hook reaches it.
#[test]
fn a_box_with_nothing_to_carry_it_or_a_card_s_is_unchanged() {
    let d = standards(&format!("<div style='margin-bottom:20px'>a{CELL}</div>"));
    assert_eq!(gaps(&d, "a"), (Some(0), Some(0)));
    let d = standards(&format!(
        "<div style='margin-top:30px'>{CELL}</div><p>b</p>"
    ));
    assert_eq!(gaps(&d, "b"), (Some(240), Some(240)));
    let card = "<div style='width:300px;background:#eee;margin-bottom:20px'>x</div>";
    for page in [
        format!("{card}<p>b</p>"),
        format!("<div style='margin-bottom:20px'>{card}</div><p>b</p>"),
    ] {
        let d = standards(&page);
        assert_eq!(tables(&d).len(), 1, "{page}");
        assert_eq!(gaps(&d, "b"), (Some(240), Some(240)), "{page}");
    }
}

// Item 3: margins through an empty block; borders separating margins.

/// An empty block's top and bottom margins collapse through it into one,
/// and that one into the margins around it: one 40px gap.
#[test]
fn an_empty_block_s_margins_collapse_through_it() {
    let d = standards("<p>a</p><div style='margin:40px 0'></div><p>b</p>");
    assert_eq!((gaps(&d, "a").1, gaps(&d, "b").0), (Some(600), Some(0)));
    // A larger one beside: the larger stands.
    let d =
        standards("<p style='margin-bottom:50px'>a</p><div style='margin:40px 0'></div><p>b</p>");
    assert_eq!((gaps(&d, "a").1, gaps(&d, "b").0), (Some(750), Some(0)));
    // An empty list or blockquote: its user-agent 1em collapses with the
    // paragraphs' 1em.
    for empty in [
        "<ul></ul>",
        "<blockquote></blockquote>",
        "<div style='margin:20px'></div>",
    ] {
        let d = standards(&format!("<p>a</p>{empty}<p>b</p>"));
        let want = if empty.contains("20px") { 300 } else { 240 };
        assert_eq!(
            (gaps(&d, "a").1, gaps(&d, "b").0),
            (Some(want), Some(0)),
            "{empty}"
        );
    }
    // At the flow's start it waits for the next paragraph.
    let d = standards("<div style='margin:40px 0'></div><p>b</p>");
    assert_eq!(gaps(&d, "b").0, Some(600));
    // Nested empty blocks: one gap, the largest.
    let d = standards(
        "<p>a</p><div style='margin-bottom:30px'><div style='margin:40px 0'></div></div><p>b</p>",
    );
    assert_eq!((gaps(&d, "a").1, gaps(&d, "b").0), (Some(600), Some(0)));
    // After a table, it goes before the next paragraph.
    let d = standards(&format!("{CELL}<div style='margin:40px 0'></div><p>b</p>"));
    assert_eq!(gaps(&d, "b").0, Some(600));
}

/// An empty `<p></p>` is an empty paragraph, as Word writes one; padding,
/// a top line or `display:none` keep an empty block's margins from
/// collapsing through it, as before.
#[test]
fn what_does_not_collapse_through_is_unchanged() {
    let d = standards("<p>a</p><p></p><p>b</p>");
    assert_eq!(all_paras(&d).len(), 3);
    assert_eq!((gaps(&d, "a").1, gaps(&d, "b").0), (Some(240), Some(0)));
    for empty in [
        "<div style='margin:40px 0;padding-top:1px'></div>",
        "<div style='margin:40px 0;border-top:1px solid #000'></div>",
        "<div style='margin:40px 0;display:none'></div>",
    ] {
        let d = standards(&format!("<p>a</p>{empty}<p>b</p>"));
        assert_eq!(
            (gaps(&d, "a").1, gaps(&d, "b").0),
            (Some(240), Some(0)),
            "{empty}"
        );
    }
}

/// A block's bottom line separates its margin from its last child's
/// (#1883, pinned here): x keeps p's 16px, then the div's 30px past the
/// line, summed.
#[test]
fn a_bottom_line_keeps_the_margins_apart() {
    let d = standards(
        "<div style='margin-bottom:30px;border-bottom:1px solid #000'><p>x</p></div><div>b</div>",
    );
    assert_eq!(gaps(&d, "x").1, Some(240 + 450));
    // With no line they collapse.
    let d = standards("<div style='margin-bottom:30px'><p>x</p></div><div>b</div>");
    assert_eq!(gaps(&d, "x").1, Some(450));
}
