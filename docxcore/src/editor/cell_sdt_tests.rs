//! Cell-level content controls (`w:sdt` around `w:tc`, #1102) under editing:
//! a click selects a placeholder, text clears its flag, and table edits keep
//! the controls balanced, each on the cells it wrapped.

use super::*;
use crate::load::{Relationships, parse_document_xml};
use crate::serialize::document_to_xml;

const CLOSE: &str = "</w:sdtContent></w:sdt>";

fn open(alias: &str, id: u32, placeholder: bool) -> String {
    let flag = if placeholder {
        "<w:showingPlcHdr/>"
    } else {
        ""
    };
    format!(
        "<w:sdt><w:sdtPr><w:alias w:val=\"{alias}\"/><w:id w:val=\"{id}\"/>{flag}\
         <w:dataBinding w:xpath=\"/ns0:{alias}[1]\"/></w:sdtPr><w:sdtContent>"
    )
}

fn cell(text: &str) -> String {
    format!(
        "<w:tc><w:tcPr><w:tcW w:w=\"2000\" w:type=\"dxa\"/></w:tcPr>\
         <w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p></w:tc>"
    )
}

fn editor(rows: &str) -> Editor {
    let xml = format!(
        "<w:document><w:body><w:tbl><w:tblPr></w:tblPr><w:tblGrid><w:gridCol w:w=\"2000\"/>\
         <w:gridCol w:w=\"2000\"/><w:gridCol w:w=\"2000\"/></w:tblGrid>{rows}</w:tbl>\
         <w:p/></w:body></w:document>"
    );
    Editor::new(parse_document_xml(&xml, &Relationships::default()))
}

/// Abstract (placeholder) over cell 0, a plain cell, Year (placeholder) over cell 2.
fn cover_row() -> Editor {
    editor(&format!(
        "<w:tr>{}{}{CLOSE}{}{}{}{CLOSE}</w:tr>",
        open("Abstract", 1, true),
        cell("[Type the abstract]"),
        cell("plain"),
        open("Year", 2, true),
        cell("[Year]"),
    ))
}

/// One control over cells 0 and 1, then a plain cell.
fn pair_row(placeholder: bool) -> Editor {
    editor(&format!(
        "<w:tr>{}{}{}{CLOSE}{}</w:tr>",
        open("Pair", 1, placeholder),
        cell("a"),
        cell("b"),
        cell("c"),
    ))
}

fn at(c: usize, offset: usize) -> Caret {
    Caret::at(vec![0, 0, c, 0], offset)
}

fn row(ed: &Editor) -> &Row {
    &ed.table(&[0]).expect("a table").rows[0]
}

fn flagged(ed: &Editor, c: usize) -> bool {
    row(ed).cells[c]
        .sdt_open
        .iter()
        .any(|o| o.contains("showingPlcHdr"))
}

fn text(ed: &Editor, c: usize) -> String {
    let Block::Paragraph(p) = &row(ed).cells[c].blocks[0] else {
        panic!("a paragraph")
    };
    p.plain_text()
}

/// The saved table is balanced, its rows' controls too, and no control id
/// is written twice.
fn assert_sound(ed: &Editor) {
    let xml = document_to_xml(&ed.doc);
    assert_eq!(
        xml.matches("<w:sdt>").count(),
        xml.matches("</w:sdt>").count(),
        "{xml}"
    );
    let t = ed.table(&[0]).expect("a table");
    assert!(t.rows.iter().all(Row::cell_sdt_balanced), "{xml}");
    let mut ids = crate::cover::sdt_ids(&xml);
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate w:id: {xml}");
    let again = parse_document_xml(&xml, &Relationships::default());
    assert_eq!(document_to_xml(&again), xml);
}

#[test]
fn a_click_in_a_cell_placeholder_selects_it_and_typing_replaces_it() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 5));
    assert!(ed.select_placeholder_at_caret());
    assert_eq!(ed.anchor, Some(at(0, 0)));
    assert_eq!(ed.caret, at(0, "[Type the abstract]".len()));
    ed.insert_str("Mine");
    assert_eq!(text(&ed, 0), "Mine");
    assert!(!flagged(&ed, 0), "typing clears the Abstract's flag");
    assert!(
        flagged(&ed, 2),
        "the Year control still shows its placeholder"
    );
    // The binding stays: Word writes the typed text back to the property.
    assert!(row(&ed).cells[0].sdt_open[0].contains("w:dataBinding"));
    assert_sound(&ed);
    // Undo brings the placeholder back, flag and all.
    while ed.undo() {}
    assert_eq!(text(&ed, 0), "[Type the abstract]");
    assert!(flagged(&ed, 0));
}

#[test]
fn a_click_outside_any_placeholder_selects_nothing() {
    let mut ed = cover_row();
    ed.set_caret(at(1, 2));
    assert_eq!(ed.placeholder_range_at(&at(1, 2)), None);
    assert!(!ed.select_placeholder_at_caret());
    assert!(!ed.has_selection());
    // A control no longer showing its placeholder is text like any other.
    let mut ed = pair_row(false);
    assert_eq!(ed.placeholder_range_at(&at(0, 0)), None);
    ed.set_caret(at(2, 0));
    assert!(!ed.select_placeholder_at_caret());
}

#[test]
fn a_placeholder_over_two_cells_selects_both_and_typing_in_the_second_clears_it() {
    let ed = pair_row(true);
    assert_eq!(
        ed.placeholder_range_at(&at(1, 1)),
        Some((at(0, 0), at(1, 1)))
    );
    let mut ed = pair_row(true);
    ed.set_caret(at(1, 1));
    ed.insert_str("x");
    assert!(
        !flagged(&ed, 0),
        "the flag is on the cell the control opens on"
    );
    assert_sound(&ed);
}

#[test]
fn backspace_and_delete_in_a_cell_placeholder_clear_only_its_flag() {
    let mut ed = cover_row();
    ed.set_caret(at(2, 6));
    ed.backspace();
    assert!(!flagged(&ed, 2));
    assert!(flagged(&ed, 0));
    let mut ed = cover_row();
    ed.set_caret(at(0, 0));
    ed.delete_forward();
    assert!(!flagged(&ed, 0));
    assert!(flagged(&ed, 2));
    // Typing in the plain cell clears neither.
    let mut ed = cover_row();
    ed.set_caret(at(1, 0));
    ed.insert_str("y");
    assert!(flagged(&ed, 0) && flagged(&ed, 2));
}

#[test]
fn a_paste_into_a_cell_placeholder_clears_its_flag() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 3));
    assert!(ed.select_placeholder_at_caret());
    ed.paste(&Clip::from_text("pasted"));
    assert_eq!(text(&ed, 0), "pasted");
    assert!(!flagged(&ed, 0) && flagged(&ed, 2));
}

#[test]
fn inline_and_block_placeholders_are_selected_by_a_click_too() {
    let xml = format!(
        "<w:document><w:body><w:p><w:r><w:t xml:space=\"preserve\">Name: </w:t></w:r>{}\
         <w:r><w:t>[Your name]</w:t></w:r>{CLOSE}<w:r><w:t xml:space=\"preserve\"> end</w:t></w:r></w:p>\
         {}<w:p><w:r><w:t>[Title]</w:t></w:r></w:p><w:p><w:r><w:t>[Sub]</w:t></w:r></w:p>{CLOSE}\
         <w:p/></w:body></w:document>",
        open("Name", 7, true),
        open("Title", 8, true),
    );
    let mut ed = Editor::new(parse_document_xml(&xml, &Relationships::default()));
    let inline = |o| Caret::at(vec![0], o);
    assert_eq!(
        ed.placeholder_range_at(&inline(9)),
        Some((inline(6), inline(17)))
    );
    assert_eq!(ed.placeholder_range_at(&inline(2)), None);
    // The block control's paragraphs sit between its two Raw boundaries.
    let title = ed
        .doc
        .body
        .iter()
        .position(|b| matches!(b, Block::Paragraph(p) if p.plain_text() == "[Title]"))
        .unwrap();
    let range = ed.placeholder_range_at(&Caret::at(vec![title + 1], 2));
    assert_eq!(
        range,
        Some((Caret::at(vec![title], 0), Caret::at(vec![title + 1], 5)))
    );
    // Typing over it clears the block control's flag.
    ed.set_caret(Caret::at(vec![title], 1));
    assert!(ed.select_placeholder_at_caret());
    ed.insert_str("T");
    let Block::Raw(raw) = &ed.doc.body[title - 1] else {
        panic!("the block control's open")
    };
    assert!(!raw.contains("showingPlcHdr"), "{raw}");
}

#[test]
fn deleting_the_second_cell_of_a_control_leaves_the_third_outside_it() {
    let mut ed = pair_row(false);
    ed.set_caret(at(1, 0));
    ed.delete_columns().unwrap();
    let r = row(&ed);
    assert_eq!(r.cells.len(), 2);
    assert_eq!(
        (r.cells[0].sdt_open.len(), r.cells[0].sdt_close.len()),
        (1, 1)
    );
    assert!(r.cells[1].sdt_open.is_empty() && r.cells[1].sdt_close.is_empty());
    assert_sound(&ed);

    let mut ed = pair_row(false);
    ed.set_caret(at(1, 0));
    ed.delete_cells(DeleteShift::ShiftLeft).unwrap();
    assert_eq!(row(&ed).cells[0].sdt_close.len(), 1);
    assert!(row(&ed).cells[1].sdt_open.is_empty());
    assert_sound(&ed);
}

#[test]
fn a_merge_touching_a_cell_level_control_is_refused() {
    // The Abstract with its plain right neighbour, both cells of a control,
    // and a plain cell with the control's last cell.
    for (ed, a, b) in [
        (cover_row(), 0, 1),
        (pair_row(false), 0, 1),
        (pair_row(false), 1, 2),
    ] {
        let mut ed = ed;
        let before = document_to_xml(&ed.doc);
        ed.anchor = Some(at(a, 0));
        ed.caret = at(b, 0);
        let err = ed.merge_cells().unwrap_err();
        assert!(err.contains("content control"), "{err}");
        assert_eq!(document_to_xml(&ed.doc), before);
        assert!(!ed.undo(), "a refusal records no step");
    }
}

#[test]
fn plain_cells_merge_in_a_row_with_a_control_elsewhere() {
    let mut ed = editor(&format!(
        "<w:tr>{}{}{}{}{CLOSE}</w:tr>",
        cell("a"),
        cell("b"),
        open("Year", 2, true),
        cell("[Year]")
    ));
    ed.anchor = Some(at(0, 0));
    ed.caret = at(1, 0);
    ed.merge_cells().unwrap();
    assert_eq!(row(&ed).cells.len(), 2);
    assert!(flagged(&ed, 1));
    assert_sound(&ed);
}

#[test]
fn split_insert_column_and_insert_row_never_copy_a_control() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 0));
    ed.split_cells(2, 1, false).unwrap();
    let r = row(&ed);
    assert_eq!(r.cells.len(), 4);
    assert_eq!(r.cells[0].sdt_open.len(), 1);
    assert!(r.cells[1].sdt_open.is_empty() && r.cells[1].sdt_close.is_empty());
    assert_sound(&ed);

    let mut ed = cover_row();
    ed.set_caret(at(0, 0));
    ed.insert_columns(false).unwrap();
    assert!(row(&ed).cells[1].sdt_open.is_empty() && row(&ed).cells[1].sdt_close.is_empty());
    assert_sound(&ed);

    for above in [false, true] {
        let mut ed = cover_row();
        ed.set_caret(at(0, 0));
        ed.insert_rows(above).unwrap();
        let t = ed.table(&[0]).unwrap();
        let wrapped: usize = t
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .map(|c| c.sdt_open.len())
            .sum();
        assert_eq!(wrapped, 2, "above {above}");
        assert_sound(&ed);
    }
}

#[test]
fn replace_into_a_cell_placeholder_clears_only_its_flag() {
    let mut ed = cover_row();
    let matches = ed.find_all("abstract", false);
    assert_eq!(matches.len(), 1);
    assert_eq!(ed.replace_matches(matches, "summary"), 1);
    assert_eq!(text(&ed, 0), "[Type the summary]");
    assert!(!flagged(&ed, 0));
    assert!(flagged(&ed, 2));
    // Replace on a selection too.
    let mut ed = cover_row();
    ed.set_caret(at(2, 1));
    assert!(ed.select_placeholder_at_caret());
    ed.replace_current_with("2027");
    assert_eq!(text(&ed, 2), "2027");
    assert!(flagged(&ed, 0) && !flagged(&ed, 2));
}

#[test]
fn delete_over_a_two_cell_placeholder_clears_its_flag() {
    let mut ed = pair_row(true);
    ed.set_caret(at(0, 0));
    assert!(ed.select_placeholder_at_caret());
    assert_eq!(
        (ed.anchor.clone(), ed.caret.clone()),
        (Some(at(0, 0)), at(1, 1))
    );
    ed.delete_forward();
    assert_eq!((text(&ed, 0), text(&ed, 1)), (String::new(), String::new()));
    assert!(!flagged(&ed, 0));
    assert_sound(&ed);
}

#[test]
fn enter_in_a_cell_placeholder_clears_its_flag() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 3));
    ed.insert_char('\n');
    assert_eq!(row(&ed).cells[0].blocks.len(), 2);
    assert!(!flagged(&ed, 0) && flagged(&ed, 2));
    assert_sound(&ed);
}

#[test]
fn a_selection_over_paragraphs_in_a_placeholder_clears_its_flag_when_deleted() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 3));
    ed.insert_char('\n');
    // Put the flag back, as a document saved like this would have it.
    let Some(Block::Table(t)) = ed.doc.body.get_mut(0) else {
        unreachable!()
    };
    t.rows[0].cells[0].sdt_open[0] = open("Abstract", 1, true);
    ed.anchor = Some(at(0, 1));
    ed.caret = Caret::at(vec![0, 0, 0, 1], 2);
    assert!(ed.delete_selection());
    assert!(!flagged(&ed, 0) && flagged(&ed, 2));
}

#[test]
fn shift_up_refuses_to_move_text_through_a_cell_level_control() {
    let mut ed = editor(&format!(
        "<w:tr>{}{}{}</w:tr><w:tr>{}{}{CLOSE}{}</w:tr>",
        cell("a"),
        cell("b"),
        cell("c"),
        open("Abstract", 1, true),
        cell("[Abstract]"),
        cell("x"),
    ));
    let before = document_to_xml(&ed.doc);
    ed.set_caret(Caret::at(vec![0, 0, 0, 0], 0));
    let err = ed.delete_cells(DeleteShift::ShiftUp).unwrap_err();
    assert!(err.contains("content control"), "{err}");
    assert_eq!(document_to_xml(&ed.doc), before);
    // A column with no control still shifts.
    ed.set_caret(Caret::at(vec![0, 0, 1, 0], 0));
    ed.delete_cells(DeleteShift::ShiftUp).unwrap();
    assert_sound(&ed);
}

#[test]
fn a_showing_placeholder_flag_turned_off_is_not_a_placeholder() {
    for (flag, shows) in [
        ("<w:showingPlcHdr w:val=\"false\"/>", false),
        ("<w:showingPlcHdr w:val=\"0\"/>", false),
        ("<w:showingPlcHdr w:val=\"off\"/>", false),
        ("<w:showingPlcHdr w:val=\"true\"/>", true),
        ("<w:showingPlcHdr w:val=\"1\"/>", true),
        ("<w:showingPlcHdr/>", true),
    ] {
        let ed = editor(&format!(
            "<w:tr><w:sdt><w:sdtPr><w:alias w:val=\"A\"/>{flag}</w:sdtPr><w:sdtContent>{}{CLOSE}</w:tr>",
            cell("[A]")
        ));
        assert_eq!(
            ed.placeholder_range_at(&at(0, 1)).is_some(),
            shows,
            "{flag}"
        );
    }
}

#[test]
fn a_placeholder_starting_in_a_nested_table_selects_nothing() {
    let inner = "<w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w=\"1000\"/></w:tblGrid>\
        <w:tr><w:tc><w:p><w:r><w:t>in</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
    let ed = editor(&format!(
        "<w:tr>{}<w:tc>{inner}<w:p><w:r><w:t>[A]</w:t></w:r></w:p></w:tc>{CLOSE}</w:tr>",
        open("A", 1, true)
    ));
    assert_eq!(
        ed.placeholder_range_at(&Caret::at(vec![0, 0, 0, 1], 1)),
        None
    );
}

#[test]
fn shift_up_refuses_the_middle_cell_of_a_three_cell_control() {
    let mut ed = editor(&format!(
        "<w:tr>{}{}{}</w:tr><w:tr>{}{}{}{}{CLOSE}</w:tr>",
        cell("a"),
        cell("b"),
        cell("c"),
        open("Wide", 1, false),
        cell("x"),
        cell("y"),
        cell("z"),
    ));
    let before = document_to_xml(&ed.doc);
    ed.set_caret(Caret::at(vec![0, 0, 1, 0], 0));
    let err = ed.delete_cells(DeleteShift::ShiftUp).unwrap_err();
    assert!(err.contains("content control"), "{err}");
    assert_eq!(document_to_xml(&ed.doc), before);
}

#[test]
fn a_two_cell_placeholder_a_vertical_merge_would_widen_selects_nothing() {
    let mut ed = pair_row(true);
    let Some(Block::Table(t)) = ed.doc.body.get_mut(0) else {
        unreachable!()
    };
    t.rows[0].cells[1].v_merge = VMerge::Restart;
    assert_eq!(ed.placeholder_range_at(&at(0, 0)), None);
    ed.set_caret(at(0, 0));
    assert!(!ed.select_placeholder_at_caret());
}

#[test]
fn joining_paragraphs_in_a_cell_placeholder_clears_its_flag() {
    for backspace in [true, false] {
        let mut ed = editor(&format!(
            "<w:tr>{}<w:tc><w:p><w:r><w:t>[One]</w:t></w:r></w:p><w:p><w:r><w:t>[Two]</w:t></w:r></w:p></w:tc>{CLOSE}{}</w:tr>",
            open("Abstract", 1, true),
            cell("x")
        ));
        if backspace {
            ed.set_caret(Caret::at(vec![0, 0, 0, 1], 0));
            ed.backspace();
        } else {
            ed.set_caret(Caret::at(vec![0, 0, 0, 0], 5));
            ed.delete_forward();
        }
        assert_eq!(row(&ed).cells[0].blocks.len(), 1, "backspace {backspace}");
        assert!(!flagged(&ed, 0), "backspace {backspace}");
        while ed.undo() {}
        assert!(flagged(&ed, 0));
    }
}

#[test]
fn change_case_in_a_cell_placeholder_clears_its_flag() {
    let mut ed = cover_row();
    ed.set_caret(at(0, 1));
    assert!(ed.select_placeholder_at_caret());
    ed.cycle_case();
    assert_ne!(text(&ed, 0), "[Type the abstract]");
    assert!(!flagged(&ed, 0) && flagged(&ed, 2));
}

#[test]
fn an_off_flag_with_spaces_around_its_equals_sign_is_off() {
    let ed = editor(&format!(
        "<w:tr><w:sdt><w:sdtPr><w:showingPlcHdr w:val = \"false\" /></w:sdtPr><w:sdtContent>{}{CLOSE}</w:tr>",
        cell("[A]")
    ));
    assert_eq!(ed.placeholder_range_at(&at(0, 1)), None);
}

#[test]
fn deleting_inside_an_inline_placeholder_clears_its_flag() {
    let xml = |_: ()| {
        format!(
            "<w:document><w:body><w:p><w:r><w:t xml:space=\"preserve\">Name: </w:t></w:r>{}\
             <w:r><w:t>[Your name]</w:t></w:r>{CLOSE}<w:r><w:t xml:space=\"preserve\"> end</w:t></w:r></w:p>\
             <w:p/></w:body></w:document>",
            open("Name", 7, true)
        )
    };
    let flag =
        |ed: &Editor| crate::serialize::blocks_to_xml(&ed.doc.body[..1]).contains("showingPlcHdr");
    let load = || Editor::new(parse_document_xml(&xml(()), &Relationships::default()));
    // Backspace and Delete inside it, and a replace over part of it.
    let mut ed = load();
    ed.set_caret(Caret::at(vec![0], 10));
    ed.backspace();
    assert!(!flag(&ed));
    let mut ed = load();
    ed.set_caret(Caret::at(vec![0], 6));
    ed.delete_forward();
    assert!(!flag(&ed));
    let mut ed = load();
    let m = ed.find_all("name]", false);
    ed.replace_matches(m, "x]");
    assert!(!flag(&ed));
    // Outside it: kept.
    let mut ed = load();
    ed.set_caret(Caret::at(vec![0], 3));
    ed.backspace();
    ed.set_caret(Caret::at(vec![0], 17));
    ed.delete_forward();
    assert!(flag(&ed));
}
