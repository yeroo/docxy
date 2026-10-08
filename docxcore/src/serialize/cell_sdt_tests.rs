//! Cell-level content controls (`<w:sdt>` wrapping `<w:tc>`) survive a save
//! (#1102): Word's cover page binds its Abstract and Year cells this way.

use super::*;
use crate::load::{Relationships, parse_document_xml};

/// A cell-level control as Word's cover page writes it: placeholder showing,
/// bound to a cover page property, locked, with an end-of-control rPr.
const ABSTRACT_OPEN: &str = "<w:sdt><w:sdtPr><w:rPr><w:color w:val=\"7F7F7F\" \
    w:themeColor=\"text1\" w:themeTint=\"80\"/></w:rPr><w:alias w:val=\"Abstract\"/>\
    <w:tag w:val=\"\"/><w:id w:val=\"8276291\"/><w:lock w:val=\"sdtLocked\"/>\
    <w:placeholder><w:docPart w:val=\"F4A1F3B2\"/></w:placeholder><w:showingPlcHdr/>\
    <w:dataBinding w:prefixMappings=\"xmlns:ns0='http://schemas.microsoft.com/office/2006/coverPageProps'\" \
    w:xpath=\"/ns0:CoverPageProperties[1]/ns0:Abstract[1]\" w:storeItemID=\"{55AF091B-3C7A-41E3-B477-F2FDAA23CFDA}\"/>\
    <w:text/></w:sdtPr><w:sdtEndPr><w:rPr><w:b/></w:rPr></w:sdtEndPr><w:sdtContent>";
const CLOSE: &str = "</w:sdtContent></w:sdt>";

fn cell(text: &str) -> String {
    format!(
        "<w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr><w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p></w:tc>"
    )
}

fn open(alias: &str, id: u32) -> String {
    format!(
        "<w:sdt><w:sdtPr><w:alias w:val=\"{alias}\"/><w:id w:val=\"{id}\"/></w:sdtPr><w:sdtContent>"
    )
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

/// `xml` is one balanced element, as Word needs it.
fn assert_well_formed(xml: &str) {
    let xml = &xml[xml.find("<w:document").expect("a document element")..];
    let mut parser = XmlParser::new(xml);
    assert!(matches!(parser.next(), Event::Start), "{xml}");
    assert!(parser.skip_element_complete(), "unbalanced: {xml}");
    assert!(
        matches!(parser.next(), Event::Eof),
        "trailing content: {xml}"
    );
}

#[test]
fn a_cell_level_control_round_trips_byte_for_byte() {
    let row = format!(
        "<w:tr><w:trPr><w:trHeight w:val=\"400\"/></w:trPr>{ABSTRACT_OPEN}{}{CLOSE}{}</w:tr>",
        cell("[Type the abstract of the document here.]"),
        cell("plain"),
    );
    let doc = load(&table(&row));
    let t = first_table(&doc);
    assert_eq!(t.rows[0].cells.len(), 2);
    assert_eq!(t.rows[0].cells[0].sdt_open, vec![ABSTRACT_OPEN.to_string()]);
    assert_eq!(t.rows[0].cells[0].sdt_close, vec![CLOSE.to_string()]);
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&row), "{saved}");
    assert_eq!(load(&saved), doc);
}

#[test]
fn a_control_over_two_cells_wraps_both() {
    let row = format!(
        "<w:tr>{}{}{}{CLOSE}{}</w:tr>",
        open("Pair", 1),
        cell("a"),
        cell("b"),
        cell("c")
    );
    let doc = load(&table(&row));
    let cells = &first_table(&doc).rows[0].cells;
    assert_eq!(cells[0].sdt_open.len(), 1);
    assert!(cells[0].sdt_close.is_empty() && cells[1].sdt_open.is_empty());
    assert_eq!(cells[1].sdt_close.len(), 1);
    assert!(document_to_xml(&doc).contains(&row));
}

#[test]
fn nested_cell_controls_keep_their_order() {
    let row = format!(
        "<w:tr>{}{}{}{CLOSE}{}{CLOSE}{}</w:tr>",
        open("Outer", 1),
        open("Inner", 2),
        cell("a"),
        cell("b"),
        cell("c")
    );
    let doc = load(&table(&row));
    let cells = &first_table(&doc).rows[0].cells;
    assert_eq!(cells[0].sdt_open, vec![open("Outer", 1), open("Inner", 2)]);
    assert_eq!(cells[0].sdt_close.len(), 1);
    assert_eq!(cells[1].sdt_close.len(), 1);
    assert!(document_to_xml(&doc).contains(&row));
}

/// The issue's table: a row-level control (around `w:tr`) and a cell-level
/// one (around `w:tc`) both keep every `w:sdtPr` child.
#[test]
fn row_and_cell_level_controls_survive_together() {
    let row_open = "<w:sdt><w:sdtPr><w:alias w:val=\"Rows\"/><w:tag w:val=\"r\"/>\
        <w:id w:val=\"5\"/><w15:repeatingSection/></w:sdtPr><w:sdtContent>";
    let rows = format!(
        "{row_open}<w:tr>{ABSTRACT_OPEN}{}{CLOSE}</w:tr>{CLOSE}<w:tr>{}</w:tr>",
        cell("Abstract"),
        cell("after")
    );
    let doc = load(&table(&rows));
    let saved = document_to_xml(&doc);
    assert!(saved.contains(&rows), "{saved}");
    assert_eq!(saved.matches("<w:sdt>").count(), 2);
    assert_eq!(saved.matches("<w:showingPlcHdr/>").count(), 1);
    assert_eq!(saved.matches("<w:dataBinding ").count(), 1);
}

#[test]
fn a_truncated_cell_control_is_closed_after_its_last_cell() {
    let xml = format!(
        "<w:document><w:body><w:tbl><w:tr>{}{}",
        open("Cut", 1),
        cell("kept")
    );
    let doc = load(&xml);
    let saved = document_to_xml(&doc);
    assert_well_formed(&saved);
    assert!(saved.contains(&format!("{}{}{CLOSE}</w:tr>", open("Cut", 1), cell("kept"))));
    assert_eq!(doc.plain_text(), "kept\n");
}

#[test]
fn a_cell_control_holding_no_cell_is_dropped_and_the_cells_kept() {
    for empty in [
        "<w:sdt><w:sdtPr><w:alias w:val=\"E\"/></w:sdtPr></w:sdt>",
        "<w:sdt><w:sdtPr/><w:sdtContent/></w:sdt>",
        "<w:sdt><w:sdtPr/><w:sdtContent></w:sdtContent></w:sdt>",
    ] {
        let row = format!("<w:tr>{}{empty}{}</w:tr>", cell("a"), cell("b"));
        let doc = load(&table(&row));
        let saved = document_to_xml(&doc);
        assert_well_formed(&saved);
        assert!(!saved.contains("w:sdt"), "{saved}");
        assert_eq!(first_table(&doc).rows[0].cells.len(), 2);
    }
}

/// An edit that left a control unclosed, or a close with nothing open, still
/// saves balanced XML.
#[test]
fn unbalanced_cell_controls_in_the_model_save_balanced() {
    let row = format!("<w:tr>{}{CLOSE}{}</w:tr>", cell("a"), cell("b"));
    let mut doc = load(&table(&format!("<w:tr>{}{}</w:tr>", cell("a"), cell("b"))));
    let Block::Table(t) = &mut doc.body[0] else {
        unreachable!()
    };
    t.rows[0].cells[0].sdt_close.push(CLOSE.into());
    t.rows[0].cells[1].sdt_open.push(open("Open", 1));
    assert!(!t.rows[0].cell_sdt_balanced());
    let saved = document_to_xml(&doc);
    assert_well_formed(&saved);
    assert!(!saved.contains(&row));
    assert!(saved.contains(&format!("{}{}{CLOSE}</w:tr>", open("Open", 1), cell("b"))));
}

fn row_of(xml: &str) -> Row {
    first_table(&load(&table(xml))).rows[0].clone()
}

#[test]
fn removing_a_cell_keeps_controls_on_the_cells_they_wrapped() {
    // A control over cells 1-2 of 3: deleting cell 2 leaves cell 3 outside it.
    let mut row = row_of(&format!(
        "<w:tr>{}{}{}{CLOSE}{}</w:tr>",
        open("Pair", 1),
        cell("a"),
        cell("b"),
        cell("c")
    ));
    row.remove_cell(1);
    assert!(row.cell_sdt_balanced());
    assert_eq!(row.cells[0].sdt_open.len(), 1);
    assert_eq!(row.cells[0].sdt_close.len(), 1);
    assert!(row.cells[1].sdt_open.is_empty() && row.cells[1].sdt_close.is_empty());

    // Deleting its first cell moves the open to the next.
    let mut row = row_of(&format!(
        "<w:tr>{}{}{}{CLOSE}{}</w:tr>",
        open("Pair", 1),
        cell("a"),
        cell("b"),
        cell("c")
    ));
    row.remove_cell(0);
    assert!(row.cell_sdt_balanced());
    assert_eq!(row.cells[0].sdt_open.len(), 1);
    assert_eq!(row.cells[0].sdt_close.len(), 1);

    // A control on just the removed cell goes with it; one around it stays.
    let mut row = row_of(&format!(
        "<w:tr>{}{}{}{}{CLOSE}{}{CLOSE}</w:tr>",
        open("Outer", 1),
        cell("a"),
        open("Inner", 2),
        cell("b"),
        cell("c")
    ));
    let gone = row.remove_cell(1);
    assert!(gone.sdt_open.is_empty() && gone.sdt_close.is_empty());
    assert!(row.cell_sdt_balanced());
    assert_eq!(row.cells[0].sdt_open, vec![open("Outer", 1)]);
    assert_eq!(row.cells[1].sdt_close.len(), 1);
}
