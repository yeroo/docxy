//! Row-level children other than cells (a comment range end between two
//! cells, a bookmark, …) survive a save where they were among the cells
//! (#1215). Word's TableComments files end a comment's range this way.

use super::*;
use crate::load::{Relationships, parse_document_xml};

const END: &str = "<w:commentRangeEnd w:id=\"0\"/>";
const MARK: &str = "<w:bookmarkStart w:id=\"7\" w:name=\"m\"/>";
const OPEN: &str = "<w:sdt><w:sdtPr><w:id w:val=\"5\"/></w:sdtPr><w:sdtContent>";
const CLOSE: &str = "</w:sdtContent></w:sdt>";

fn cell(text: &str) -> String {
    format!("<w:tc><w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p></w:tc>")
}

fn table(rows: &str) -> String {
    format!(
        "<w:document><w:body><w:tbl><w:tblPr></w:tblPr><w:tblGrid></w:tblGrid>{rows}</w:tbl></w:body></w:document>"
    )
}

fn load(xml: &str) -> Document {
    parse_document_xml(xml, &Relationships::default())
}

fn first_table(doc: &Document) -> &Table {
    match &doc.body[0] {
        Block::Table(t) => t,
        other => panic!("expected a table, got {other:?}"),
    }
}

#[test]
fn a_comment_range_end_between_cells_stays_between_them() {
    let row = format!("<w:tr>{}{END}{}</w:tr>", cell("A"), cell("B"));
    let doc = load(&table(&row));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells.len(), 2);
    assert_eq!(t.rows[0].cells[1].raw_before, vec![END.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&format!("</w:tc>{END}<w:tc>")), "{saved}");
    assert!(saved.contains(&row), "{saved}");
    assert_eq!(load(&saved), doc);
}

#[test]
fn markers_before_the_first_cell_and_after_the_last_keep_their_place() {
    let row = format!(
        "<w:tr><w:trPr><w:trHeight w:val=\"400\"/></w:trPr>{MARK}{}{}{END}</w:tr>",
        cell("A"),
        cell("B")
    );
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 2);
    assert_eq!(r.cells[0].raw_before, vec![MARK.to_string()]);
    assert_eq!(r.raw_end, vec![END.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
    assert_eq!(load(&saved), doc);
}

#[test]
fn a_marker_inside_a_cell_level_control_stays_inside_it() {
    let row = format!("<w:tr>{OPEN}{MARK}{}{CLOSE}{}</w:tr>", cell("A"), cell("B"));
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 2);
    assert_eq!(r.cells[0].raw_before, vec![MARK.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
}

#[test]
fn a_marker_after_a_controls_last_cell_moves_past_its_close() {
    let row = format!("<w:tr>{OPEN}{}{END}{CLOSE}{}</w:tr>", cell("A"), cell("B"));
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 2);
    assert_eq!(r.cells[1].raw_before, vec![END.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(
        saved.contains(&format!("{CLOSE}{END}{}", cell("B"))),
        "{saved}"
    );
}

#[test]
fn a_marker_in_a_control_without_cells_is_kept() {
    let row = format!("<w:tr>{}{OPEN}{END}{CLOSE}</w:tr>", cell("A"));
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 1);
    assert_eq!(r.raw_end, vec![END.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(
        saved.contains(&format!("{}{END}</w:tr>", cell("A"))),
        "{saved}"
    );
}

#[test]
fn row_level_custom_xml_is_still_skipped() {
    let row = format!(
        "<w:tr>{}<w:customXml w:element=\"x\">{}</w:customXml></w:tr>",
        cell("A"),
        cell("B")
    );
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 1);
    assert!(r.raw_end.is_empty());
    assert!(!document_to_xml(&doc).contains("customXml"));
}

#[test]
fn an_inserted_row_does_not_repeat_the_template_rows_markers() {
    use crate::editor::{Caret, Editor};
    let row = format!("<w:tr>{}{MARK}{}{END}</w:tr>", cell("A"), cell("B"));
    for above in [false, true] {
        let mut ed = Editor::new(load(&table(&row)));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 0);
        ed.insert_rows(above).unwrap();
        assert_eq!(first_table(&ed.doc).rows.len(), 2);
        let saved = document_to_xml(&ed.doc);
        assert_eq!(saved.matches(END).count(), 1, "{saved}");
        assert_eq!(saved.matches(MARK).count(), 1, "{saved}");
    }
}

#[test]
fn removing_a_cell_keeps_the_markers_before_it_between_its_neighbours() {
    let row = format!(
        "<w:tr>{}{MARK}{}{END}{}</w:tr>",
        cell("A"),
        cell("B"),
        cell("C")
    );
    let doc = load(&table(&row));
    let mut r = first_table(&doc).rows[0].clone();
    let gone = r.remove_cell(1);
    assert!(gone.raw_before.is_empty());
    assert_eq!(
        r.cells[1].raw_before,
        vec![MARK.to_string(), END.to_string()]
    );
    // The last cell's markers go to the row's end, ahead of those there.
    let mut r = first_table(&doc).rows[0].clone();
    r.raw_end.push("<w:bookmarkEnd w:id=\"7\"/>".to_string());
    r.remove_cell(2);
    assert_eq!(
        r.raw_end,
        vec![END.to_string(), "<w:bookmarkEnd w:id=\"7\"/>".to_string()]
    );
    assert_eq!(r.cells.len(), 2);
    let saved = document_to_xml(&Document {
        body: vec![Block::Table(Table {
            rows: vec![r],
            ..Default::default()
        })],
    });
    assert!(
        saved.contains(&format!(
            "{MARK}{}{END}<w:bookmarkEnd w:id=\"7\"/></w:tr>",
            cell("B")
        )),
        "{saved}"
    );
}
