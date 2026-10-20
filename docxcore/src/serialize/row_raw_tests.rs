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

const XOPEN: &str = "<w:customXml w:uri=\"urn:x\" w:element=\"x\"><w:customXmlPr><w:attr w:name=\"a\" w:val=\"1\"/></w:customXmlPr>";
const XCLOSE: &str = "</w:customXml>";

fn custom_xml_row(inner: &str) -> String {
    format!("<w:tr>{XOPEN}{inner}{XCLOSE}</w:tr>")
}

#[test]
fn row_level_custom_xml_keeps_its_cells_and_round_trips() {
    let row = custom_xml_row(&format!("{}{}", cell("A"), cell("B")));
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 2);
    assert_eq!(r.cells[0].sdt_open, vec![XOPEN.to_string()]);
    assert_eq!(r.cells[1].sdt_close, vec![XCLOSE.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
    assert_eq!(load(&saved), doc);
}

#[test]
fn row_level_custom_xml_without_properties_or_around_one_cell() {
    let row = format!(
        "<w:tr><w:customXml w:element=\"y\">{}</w:customXml>{}</w:tr>",
        cell("A"),
        cell("B")
    );
    let doc = load(&table(&row));
    assert_eq!(first_table(&doc).rows[0].cells.len(), 2);
    assert!(document_to_xml(&doc).contains(&row));
}

#[test]
fn custom_xml_nests_with_content_controls_in_either_order() {
    let a = format!(
        "<w:tr>{XOPEN}{OPEN}{}{}{CLOSE}{XCLOSE}</w:tr>",
        cell("A"),
        cell("B")
    );
    let b = format!(
        "<w:tr>{OPEN}{XOPEN}{}{}{XCLOSE}{CLOSE}</w:tr>",
        cell("A"),
        cell("B")
    );
    let c = format!(
        "<w:tr>{XOPEN}{}{OPEN}{}{CLOSE}{XCLOSE}</w:tr>",
        cell("A"),
        cell("B")
    );
    for row in [a, b, c] {
        let doc = load(&table(&row));
        assert_eq!(first_table(&doc).rows[0].cells.len(), 2);
        let saved = document_to_xml(&doc);
        assert!(saved.contains(&row), "{saved}");
        assert_eq!(load(&saved), doc);
    }
}

#[test]
fn markers_between_cells_inside_custom_xml_stay_inside_it() {
    let row = custom_xml_row(&format!("{}{END}{}", cell("A"), cell("B")));
    let doc = load(&table(&row));
    assert_eq!(
        first_table(&doc).rows[0].cells[1].raw_before,
        vec![END.to_string()]
    );
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
    assert_eq!(load(&saved), doc);
}

#[test]
fn custom_xml_holding_no_cell_is_dropped_and_its_markers_kept() {
    let row = format!(
        "<w:tr>{}{XOPEN}{END}{XCLOSE}{}</w:tr>",
        cell("A"),
        cell("B")
    );
    let doc = load(&table(&row));
    let r = &first_table(&doc).rows[0];
    assert_eq!(r.cells.len(), 2);
    assert_eq!(r.cells[1].raw_before, vec![END.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(!saved.contains("customXml"), "{saved}");
    assert_eq!(saved.matches(END).count(), 1, "{saved}");
}

#[test]
fn truncated_custom_xml_is_closed_after_its_last_cell() {
    let xml = format!("<w:document><w:body><w:tbl><w:tr>{XOPEN}{}", cell("A"));
    let saved = document_to_xml(&load(&xml));
    assert!(
        saved.contains(&format!("{XOPEN}{}{XCLOSE}</w:tr>", cell("A"))),
        "{saved}"
    );
}

#[test]
fn a_custom_xml_open_left_without_its_close_is_closed_at_the_rows_end() {
    let doc = load(&table(&custom_xml_row(&format!(
        "{}{}",
        cell("A"),
        cell("B")
    ))));
    let mut t = first_table(&doc).clone();
    t.rows[0].cells[1].sdt_close.clear();
    t.rows[0].raw_end.push(END.to_string());
    let saved = document_to_xml(&Document {
        body: vec![Block::Table(t)],
    });
    assert!(
        saved.contains(&format!("{}{END}{XCLOSE}</w:tr>", cell("B"))),
        "{saved}"
    );
    assert_eq!(saved.matches(XCLOSE).count(), 1, "{saved}");
}

#[test]
fn removing_a_cell_keeps_a_custom_xml_balanced_and_not_stretched() {
    let row = format!("{XOPEN}{}{}{XCLOSE}{}", cell("A"), cell("B"), cell("C"));
    let doc = load(&table(&format!("<w:tr>{row}</w:tr>")));
    // The second cell: the wrapper shrinks to the first.
    let mut r = first_table(&doc).rows[0].clone();
    r.remove_cell(1);
    let saved = document_to_xml(&Document {
        body: vec![Block::Table(Table {
            rows: vec![r],
            ..Default::default()
        })],
    });
    assert!(
        saved.contains(&format!(
            "<w:tr>{XOPEN}{}{XCLOSE}{}</w:tr>",
            cell("A"),
            cell("C")
        )),
        "{saved}"
    );
    // The first cell: the wrapper now opens on the second.
    let mut r = first_table(&doc).rows[0].clone();
    r.remove_cell(0);
    assert!(r.cell_sdt_balanced());
    assert_eq!(r.cells[0].sdt_open, vec![XOPEN.to_string()]);
}

#[test]
fn merge_and_shift_up_stay_refused_over_a_custom_xml_cell() {
    use crate::editor::{Caret, Editor};
    let row = custom_xml_row(&format!("{}{}", cell("A"), cell("B")));
    let mut ed = Editor::new(load(&table(&row)));
    ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
    ed.select_cells(&[0], (0, 0), (0, 1));
    let before = ed.doc.clone();
    assert!(ed.merge_cells().is_err());
    assert_eq!(ed.doc, before);
}

#[test]
fn custom_xml_mentioning_the_placeholder_flag_round_trips_untouched() {
    let row = format!(
        "<w:tr><w:customXml w:element=\"x\" w:showingPlcHdr=\"1\">{}</w:customXml></w:tr>",
        cell("A")
    );
    let doc = load(&table(&row));
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
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

/// Rows "A<i>" / "B<i>" with a distinct marker between the cells and after them.
fn marked_rows(n: usize) -> String {
    (0..n)
        .map(|i| {
            format!(
                "<w:tr>{}<w:bookmarkStart w:id=\"{i}\" w:name=\"m{i}\"/>{}<w:commentRangeEnd w:id=\"{i}\"/></w:tr>",
                cell(&format!("A{i}")),
                cell(&format!("B{i}"))
            )
        })
        .collect()
}

fn marker_ids(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in xml.match_indices("<w:commentRangeEnd w:id=\"") {
        let rest = &xml[at + 25..];
        out.push(rest[..rest.find('"').unwrap()].to_string());
    }
    out
}

fn delete_rows_of(n: usize, top: usize, bottom: usize) -> crate::editor::Editor {
    use crate::editor::{Caret, Editor};
    let mut ed = Editor::new(load(&table(&marked_rows(n))));
    ed.caret = Caret::at(vec![0, top, 0, 0], 0);
    ed.select_cells(&[0], (top, 0), (bottom, 1));
    ed.delete_rows().unwrap();
    ed
}

#[test]
fn deleting_a_middle_row_moves_its_markers_to_the_row_before() {
    let ed = delete_rows_of(3, 1, 1);
    let t = first_table(&ed.doc);
    assert_eq!(t.rows.len(), 2);
    // Its own range end, then the moved bookmark and range end.
    assert_eq!(t.rows[0].raw_end.len(), 3);
    let saved = document_to_xml(&ed.doc);
    assert_eq!(marker_ids(&saved), ["0", "1", "2"], "{saved}");
    assert_eq!(saved.matches("w:name=\"m1\"").count(), 1, "{saved}");
    assert_eq!(load(&saved), ed.doc);
}

#[test]
fn deleting_the_last_row_moves_its_markers_to_the_row_before() {
    let ed = delete_rows_of(3, 2, 2);
    let saved = document_to_xml(&ed.doc);
    assert_eq!(marker_ids(&saved), ["0", "1", "2"], "{saved}");
    assert_eq!(load(&saved), ed.doc);
}

#[test]
fn deleting_the_first_row_moves_its_markers_before_the_next_rows_first_cell() {
    let ed = delete_rows_of(3, 0, 0);
    let t = first_table(&ed.doc);
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0].cells[0].raw_before.len(), 2);
    let saved = document_to_xml(&ed.doc);
    assert_eq!(marker_ids(&saved), ["0", "1", "2"], "{saved}");
    assert_eq!(load(&saved), ed.doc);
}

#[test]
fn deleting_several_rows_keeps_every_marker_once_in_order() {
    for (top, bottom) in [(1, 2), (0, 1)] {
        let ed = delete_rows_of(4, top, bottom);
        assert_eq!(first_table(&ed.doc).rows.len(), 2);
        let saved = document_to_xml(&ed.doc);
        assert_eq!(
            marker_ids(&saved),
            ["0", "1", "2", "3"],
            "{top}..{bottom} {saved}"
        );
        assert_eq!(load(&saved), ed.doc);
    }
}

#[test]
fn removing_a_row_hands_back_a_row_without_the_markers() {
    let doc = load(&table(&marked_rows(2)));
    let mut t = first_table(&doc).clone();
    let gone = t.remove_row(1).unwrap();
    assert!(gone.raw_end.is_empty());
    assert!(gone.cells.iter().all(|c| c.raw_before.is_empty()));
}

#[test]
fn delete_cells_shift_left_over_every_cell_of_a_row_keeps_the_markers() {
    use crate::editor::{Caret, DeleteShift, Editor};
    let mut ed = Editor::new(load(&table(&marked_rows(3))));
    ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
    ed.select_cells(&[0], (1, 0), (1, 1));
    ed.delete_cells(DeleteShift::ShiftLeft).unwrap();
    assert_eq!(first_table(&ed.doc).rows.len(), 2);
    let saved = document_to_xml(&ed.doc);
    assert_eq!(marker_ids(&saved), ["0", "1", "2"], "{saved}");
}

#[test]
fn delete_column_that_empties_a_row_keeps_its_markers() {
    use crate::editor::{Caret, Editor};
    let mut doc = load(&table(&marked_rows(3)));
    let Block::Table(t) = &mut doc.body[0] else {
        panic!()
    };
    // Row 1 has one cell spanning... leave it with a single cell.
    t.rows[1].remove_cell(1);
    let mut ed = Editor::new(doc);
    ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
    ed.select_cells(&[0], (1, 0), (1, 0));
    ed.delete_columns().unwrap();
    let saved = document_to_xml(&ed.doc);
    assert_eq!(marker_ids(&saved), ["0", "1", "2"], "{saved}");
}

#[test]
fn inserting_columns_splitting_the_table_and_splitting_cells_never_repeat_a_marker() {
    use crate::editor::{Caret, Editor};
    let fresh = || {
        let mut ed = Editor::new(load(&table(&marked_rows(2))));
        ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
        ed
    };
    let count = |ed: &Editor| {
        let saved = document_to_xml(&ed.doc);
        (
            saved.matches("<w:commentRangeEnd").count(),
            saved.matches("<w:bookmarkStart").count(),
        )
    };
    for left in [false, true] {
        let mut ed = fresh();
        ed.insert_columns(left).unwrap();
        assert_eq!(count(&ed), (2, 2));
    }
    let mut ed = fresh();
    ed.split_table().unwrap();
    assert_eq!(count(&ed), (2, 2));
    let mut ed = fresh();
    ed.select_cells(&[0], (0, 0), (1, 1));
    ed.merge_cells().unwrap();
    assert_eq!(count(&ed), (2, 2));
    ed.split_cells(2, 2, false).unwrap();
    assert_eq!(count(&ed), (2, 2));
}
