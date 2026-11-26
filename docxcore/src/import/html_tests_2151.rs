//! The leftovers of #2114 (#2151), each laid out as a browser does: a
//! block's vertical box next to a child table, an empty block's margins
//! collapsing through it, a table in an indented block laid out in its
//! content box, and a padding-less card at a quirky cell edge.

use super::*;
use crate::model::{Block as DocBlock, Table};

fn doc(html: &str) -> Document {
    import_html(html.as_bytes()).unwrap()
}

/// The same page in standards mode.
fn standards(html: &str) -> Document {
    doc(&format!("<!DOCTYPE html>{html}"))
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
