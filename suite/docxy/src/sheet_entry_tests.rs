//! The sheet tab's cell entry (#599 #653 #654 #658 #673) and Excel's editor
//! keys and entry shortcuts (#662 #663), exercised on `SheetView` without a
//! window. `sheet_key` only routes to these methods; `uiharness/cases/
//! sheet-edit.uit` drives the same steps through real keystrokes.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::{Cell, CellValue, Xf};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn put(v: &mut SheetView, r: u32, c: u32, cell: Cell) {
    let s = v.active;
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

/// Type `text` into the selected cell the way keystrokes do: a fresh
/// Enter-mode editor, then each character.
fn type_fresh(v: &mut SheetView, text: &str) {
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    for ch in text.chars() {
        v.edit_type(&ch.to_string());
    }
}

fn value(v: &SheetView, r: u32, c: u32) -> CellValue {
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn xf(v: &SheetView, r: u32, c: u32) -> Xf {
    let style = v.sheet().cell(r, c).map(|c| c.style).unwrap_or(0);
    v.pkg.workbook.styles.xf(style)
}

fn select(v: &mut SheetView, r: u32, c: u32) {
    v.sel = (r, c);
    v.anchor = (r, c);
}

// ---- #653 / #654 / #599 / #658: what a commit stores --------------------------

#[test]
fn a_typed_entry_is_recognised_and_formatted() {
    let mut v = view();
    for (r, text) in ["1,234", "$1,234.56", "12.5%", "1/15/2024", "9:30 PM", "1E3"]
        .iter()
        .enumerate()
    {
        select(&mut v, r as u32, 0);
        type_fresh(&mut v, text);
        assert!(v.commit_edit(), "{text}");
    }
    assert_eq!(value(&v, 0, 0), CellValue::Number(1234.0));
    assert_eq!(xf(&v, 0, 0).code.as_deref(), Some("#,##0"));
    assert_eq!(value(&v, 1, 0), CellValue::Number(1234.56));
    assert_eq!(value(&v, 2, 0), CellValue::Number(0.125));
    assert_eq!(v.cell_text(2, 0), "12.50%");
    assert_eq!(value(&v, 3, 0), CellValue::Number(45306.0));
    assert_eq!(v.cell_text(3, 0), "1/15/2024");
    assert_eq!(v.cell_text(4, 0), "9:30 PM");
    assert_eq!(v.cell_text(5, 0), "1.00E+03");
}

#[test]
fn a_text_cell_keeps_the_entry_and_a_percent_cell_divides_it() {
    let mut v = view();
    let text = v.pkg.workbook.styles.intern(Xf {
        numfmt: gridcore::sheet::NumFmt::Text,
        code: Some("@".into()),
        ..Xf::default()
    });
    let pct = v.pkg.workbook.styles.intern(Xf {
        numfmt: gridcore::sheet::NumFmt::Percent { decimals: 2 },
        code: Some("0.00%".into()),
        ..Xf::default()
    });
    for (r, entry) in ["007", "1/15/2024", "=1+2", "TRUE"].iter().enumerate() {
        put(
            &mut v,
            r as u32,
            0,
            Cell {
                style: text,
                ..Cell::default()
            },
        );
        select(&mut v, r as u32, 0);
        type_fresh(&mut v, entry);
        v.commit_edit();
        assert_eq!(value(&v, r as u32, 0), CellValue::Text(entry.to_string()));
    }
    for (r, entry, want) in [
        (0, "12.5", 0.125),
        (1, "1", 0.01),
        (2, "-3", -0.03),
        (3, "0.5", 0.5),
    ] {
        put(
            &mut v,
            r,
            2,
            Cell {
                style: pct,
                ..Cell::default()
            },
        );
        select(&mut v, r, 2);
        type_fresh(&mut v, entry);
        v.commit_edit();
        assert_eq!(value(&v, r, 2), CellValue::Number(want), "{entry}");
    }
}

#[test]
fn an_apostrophe_is_a_quote_prefix_and_an_untouched_reedit_is_a_no_op() {
    let mut v = view();
    type_fresh(&mut v, "'007");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Text("007".into()));
    assert!(xf(&v, 0, 0).quote_prefix);
    assert_eq!(v.edit_string(0, 0), "'007");
    let undo = v.undo.len();
    v.begin_cell_edit(None);
    assert_eq!(v.editing.as_deref(), Some("'007"));
    assert!(!v.commit_edit());
    assert_eq!(v.undo.len(), undo);
    // Plain input clears the prefix.
    type_fresh(&mut v, "7");
    v.commit_edit();
    assert_eq!(value(&v, 0, 0), CellValue::Number(7.0));
    assert!(!xf(&v, 0, 0).quote_prefix);
}

#[test]
fn an_entry_over_the_cell_limit_is_refused_and_the_editor_stays() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::text("old"));
    v.begin_cell_edit(Some("y".repeat(32_768)));
    assert!(!v.commit_edit());
    assert!(v.editing.is_some(), "the editor stays open");
    assert!(
        v.entry_error
            .as_deref()
            .is_some_and(|e| e.contains("32767"))
    );
    assert_eq!(value(&v, 0, 0), CellValue::Text("old".into()));
    assert!(v.undo.is_empty());
    v.editing = Some("y".repeat(32_767));
    assert!(v.commit_edit());
}

// ---- #673 ------------------------------------------------------------------

/// Save a workbook whose A1 is `=DATE(2024,1,1)-DATE(2024,1,2)` formatted
/// `yyyy-mm-dd`, with or without the cached `<v>-1</v>`, and open it.
fn negative_date_book(cached: bool) -> SheetView {
    let mut pkg = gridcore::xlsx::new_xlsx();
    let style = pkg.workbook.styles.intern(Xf {
        numfmt: gridcore::sheet::NumFmt::Date,
        code: Some("yyyy-mm-dd".into()),
        ..Xf::default()
    });
    pkg.workbook.sheets[0].set_cell(
        0,
        0,
        Cell {
            value: if cached {
                CellValue::Number(-1.0)
            } else {
                CellValue::Empty
            },
            style,
            ..Cell::formula("DATE(2024,1,1)-DATE(2024,1,2)")
        },
    );
    pkg.workbook.sheets[0].set_cell(0, 1, Cell::formula("A1"));
    let dir = std::env::temp_dir().join(format!("docxy-673-{}-{cached}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("neg-date.xlsx");
    std::fs::write(&path, gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    let (surface, status) = sheet_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let Surface::Sheet(v) = surface else {
        panic!("{status}")
    };
    v
}

#[test]
fn a_negative_date_shows_hashes_whether_or_not_the_file_cached_it() {
    // Before #673 a file without the cached value (openpyxl's kind) opened
    // blank: the suite never computed it. With the cached -1 it showed "-1".
    for cached in [false, true] {
        let v = negative_date_book(cached);
        assert_eq!(value(&v, 0, 0), CellValue::Number(-1.0), "cached {cached}");
        assert_eq!(v.cell_text(0, 0), "########", "cached {cached}");
        // B1 (=A1, General) still shows -1.
        assert_eq!(v.cell_text(0, 1), "-1", "cached {cached}");
    }
}

// ---- #662: editor keys ---------------------------------------------------------

#[test]
fn f2_while_typing_keeps_the_text_and_word_keys_edit_it() {
    // Issue step 1: type "one two three", F2, Ctrl+Left, Ctrl+Delete.
    let mut v = view();
    select(&mut v, 4, 0);
    type_fresh(&mut v, "one two three");
    assert_eq!(v.edit_mode, EditMode::Enter);
    v.edit_toggle_mode();
    assert_eq!(v.edit_mode, EditMode::Edit);
    assert_eq!(v.editing.as_deref(), Some("one two three"));
    v.edit_word_move(false);
    assert_eq!(v.edit_caret, 8);
    v.edit_delete_to_end();
    assert_eq!(v.editing.as_deref(), Some("one two "));
    v.edit_word_move(false);
    assert_eq!(v.edit_caret, 4);
    v.edit_word_move(true);
    assert_eq!(v.edit_caret, 8);
    // F2 again returns to Enter mode.
    v.edit_toggle_mode();
    assert_eq!(v.edit_mode, EditMode::Enter);
}

#[test]
fn enter_mode_arrows_commit_except_where_a_formula_points() {
    // Enter mode: the arrows never move the caret. Where a formula wants a
    // reference next they point at a cell; anywhere else they commit and move.
    let mut v = view();
    type_fresh(&mut v, "abc");
    assert!(!v.edit_arrows_move_caret());
    assert!(!v.edit_point(0, 1, false), "text never points");
    for formula in ["=A1+", "-L1-", "+A99*", "=SUM(", "=A1:"] {
        type_fresh(&mut v, formula);
        assert!(!v.edit_arrows_move_caret(), "{formula}");
        assert!(v.edit_point(0, 1, false), "{formula} points");
    }
    // A formula whose caret sits after a value, not an operator, doesn't.
    for formula in ["=A1", "=5", "-L1", "=SUM(A1)"] {
        type_fresh(&mut v, formula);
        assert!(!v.edit_point(0, 1, false), "{formula} commits");
        assert_eq!(v.editing.as_deref(), Some(formula), "left alone");
    }
    // `-5` is a number, which is exactly why it has nothing to point after.
    type_fresh(&mut v, "-5");
    assert!(!v.edit_is_formula(), "a number is not a formula");
    assert!(!v.edit_point(0, 1, false));
    // Edit mode: the arrows move the caret and never point.
    v.edit_mode = EditMode::Edit;
    v.editing = Some("=A1+".into());
    assert!(v.edit_arrows_move_caret());
    assert!(!v.edit_point(0, 1, false));
    // F2 / a double-click / the fx bar open in Edit mode.
    v.end_cell_edit();
    v.begin_cell_edit(None);
    assert_eq!(v.edit_mode, EditMode::Edit);
}

// ---- #757: pointing at cells with the arrows, and F4's anchoring ------------

#[test]
fn arrows_point_at_cells_from_the_edited_cell() {
    // From A1: `=`, Down, Right → B2 (each arrow moves the pointed cell, the
    // caret staying after it); Shift+Right grows it into a range.
    let mut v = view();
    type_fresh(&mut v, "=");
    assert!(v.edit_point(1, 0, false));
    assert_eq!(v.editing.as_deref(), Some("=A2"));
    assert!(v.edit_point(0, 1, false));
    assert_eq!(v.editing.as_deref(), Some("=B2"));
    assert_eq!(v.edit_caret, 3);
    assert!(v.edit_point(0, 1, true));
    assert_eq!(v.editing.as_deref(), Some("=B2:C2"));
    assert!(v.edit_point(1, 0, true));
    assert_eq!(v.editing.as_deref(), Some("=B2:C3"));
    // A plain arrow after a Shift one collapses back to one moving cell.
    assert!(v.edit_point(0, 1, false));
    assert_eq!(v.editing.as_deref(), Some("=D3"));
    // Nothing moved the selection or closed the editor.
    assert_eq!(v.sel, (0, 0));
    assert_eq!(v.edit_origin, Some((0, 0, 0)));
}

#[test]
fn arrows_point_after_an_operator_or_a_bracket() {
    let mut v = view();
    type_fresh(&mut v, "=SUM(");
    assert!(v.edit_point(1, 0, false));
    assert_eq!(v.editing.as_deref(), Some("=SUM(A2"));
    // Typing ends the pointing: `)` then an arrow commits, it doesn't point.
    v.edit_point = None; // what `sheet_key` does on any other key
    v.edit_type(")");
    assert!(!v.edit_point(1, 0, false));

    let mut v = view();
    type_fresh(&mut v, "=A1+");
    assert!(v.edit_point(0, 1, false));
    assert_eq!(v.editing.as_deref(), Some("=A1+B1"));

    // Text after the caret stays where it was.
    let mut v = view();
    type_fresh(&mut v, "=SUM()");
    v.edit_caret = 5;
    assert!(v.edit_point(0, 2, false));
    assert_eq!(v.editing.as_deref(), Some("=SUM(C1)"));
    assert_eq!(v.edit_caret, 7);

    // Moving the caret by hand ends the pointing too.
    let mut v = view();
    type_fresh(&mut v, "=1+");
    assert!(v.edit_point(0, 1, false));
    v.edit_caret = 2;
    assert!(!v.edit_point(0, 1, false), "after `1`, nothing is pointed");
    assert_eq!(v.editing.as_deref(), Some("=1+B1"));
}

#[test]
fn pointing_after_a_colon_moves_the_second_end() {
    // From C1, `=SUM(A1:` then Down twice: A1 stays, the second end walks.
    let mut v = view();
    select(&mut v, 0, 2);
    type_fresh(&mut v, "=SUM(A1:");
    assert!(v.edit_point(1, 0, false));
    assert_eq!(v.editing.as_deref(), Some("=SUM(A1:C2"));
    assert!(v.edit_point(1, 0, false));
    assert_eq!(v.editing.as_deref(), Some("=SUM(A1:C3"));
    // Shift there has no range of its own to grow: it moves that end too.
    assert!(v.edit_point(0, 1, true));
    assert_eq!(v.editing.as_deref(), Some("=SUM(A1:D3"));
}

#[test]
fn pointing_stops_at_the_sheet_edge() {
    let mut v = view();
    type_fresh(&mut v, "=");
    assert!(v.edit_point(-1, 0, false));
    assert_eq!(v.editing.as_deref(), Some("=A1"));
    assert!(v.edit_point(0, -1, false));
    assert_eq!(v.editing.as_deref(), Some("=A1"));
}

#[test]
fn an_arrow_after_a_finished_formula_commits_and_moves() {
    // `=5` then Right: no reference is wanted there, so the arrow does what
    // it does after any entry — commit and move (`sheet_key`'s fallback).
    let mut v = view();
    type_fresh(&mut v, "=5");
    assert!(!v.edit_point(0, 1, false));
    assert_eq!(v.commit_and_move(0, 1), Some(true));
    assert!(v.editing.is_none());
    assert_eq!(value(&v, 0, 0), CellValue::Number(5.0));
    assert_eq!(v.sel, (0, 1));
}

#[test]
fn edit_mode_arrows_move_the_caret() {
    let mut v = view();
    put(
        &mut v,
        0,
        0,
        Cell {
            formula: Some("B1+".into()),
            ..Cell::default()
        },
    );
    v.begin_cell_edit(None);
    v.edit_caret_to_end();
    assert_eq!(v.editing.as_deref(), Some("=B1+"));
    assert!(v.edit_arrows_move_caret());
    assert!(!v.edit_point(0, -1, false));
    v.edit_move(-1);
    assert_eq!(v.edit_caret, 3);
    assert_eq!(v.editing.as_deref(), Some("=B1+"));
}

#[test]
fn f4_cycles_the_reference_at_the_caret_only_in_a_formula() {
    let mut v = view();
    type_fresh(&mut v, "=A1");
    for want in ["=$A$1", "=A$1", "=$A1", "=A1"] {
        assert!(v.edit_cycle_ref());
        assert_eq!(v.editing.as_deref(), Some(want));
        assert_eq!(v.edit_caret, want.chars().count());
    }
    // Not on a reference: nothing happens.
    type_fresh(&mut v, "=SUM(");
    assert!(!v.edit_cycle_ref());
    assert_eq!(v.editing.as_deref(), Some("=SUM("));
    // Not a formula: nothing happens, even to text that reads like a cell.
    type_fresh(&mut v, "A1");
    assert!(!v.edit_cycle_ref());
    assert_eq!(v.editing.as_deref(), Some("A1"));
    // A pointed reference can be anchored straight away.
    type_fresh(&mut v, "=");
    assert!(v.edit_point(1, 1, false));
    assert!(v.edit_cycle_ref());
    assert_eq!(v.editing.as_deref(), Some("=$B$2"));
}

#[test]
fn insert_toggles_overtype() {
    // Issue step 2: A6 abcd, F2, Home, Insert, XY → XYcd.
    let mut v = view();
    put(&mut v, 5, 0, Cell::text("abcd"));
    select(&mut v, 5, 0);
    v.begin_cell_edit(None);
    v.edit_caret = 0;
    v.edit_overtype = true;
    v.edit_type("X");
    v.edit_type("Y");
    assert_eq!(v.editing.as_deref(), Some("XYcd"));
    // Overtype ends with the editor.
    v.end_cell_edit();
    assert!(!v.edit_overtype);
}

#[test]
fn alt_enter_inserts_a_line_feed_and_the_commit_wraps_the_cell() {
    // Issue step 3.
    let mut v = view();
    type_fresh(&mut v, "ab");
    v.edit_type("\n");
    assert!(v.editing.is_some(), "still editing");
    v.edit_type("cd");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Text("ab\ncd".into()));
    assert!(xf(&v, 0, 0).wrap);
}

#[test]
fn backspace_opens_an_empty_editor_that_escape_undoes() {
    // Issue step 4: what `sheet_key` does for Backspace outside an editor.
    let mut v = view();
    let bold = v.pkg.workbook.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    put(
        &mut v,
        0,
        0,
        Cell {
            style: bold,
            ..Cell::text("Alpha")
        },
    );
    v.begin_cell_edit(Some(String::new()));
    assert_eq!(v.editing.as_deref(), Some(""));
    assert_eq!(
        value(&v, 0, 0),
        CellValue::Text("Alpha".into()),
        "nothing written yet"
    );
    v.end_cell_edit(); // Esc
    assert_eq!(value(&v, 0, 0), CellValue::Text("Alpha".into()));
    // Enter on the empty editor clears the content and keeps the style.
    v.begin_cell_edit(Some(String::new()));
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Empty);
    assert!(xf(&v, 0, 0).bold);
}

#[test]
fn ctrl_z_while_editing_undoes_only_the_typing() {
    // Issue step 5.
    let mut v = view();
    type_fresh(&mut v, "1");
    v.commit_edit();
    let undo = v.undo.len();
    select(&mut v, 0, 5);
    type_fresh(&mut v, "abc");
    v.edit_revert();
    assert_eq!(v.editing.as_deref(), Some(""), "the editor stays, empty");
    assert_eq!(v.undo.len(), undo);
    assert_eq!(value(&v, 0, 0), CellValue::Number(1.0));
    // An F2 editor goes back to the cell's own text.
    select(&mut v, 0, 0);
    v.begin_cell_edit(None);
    v.edit_type("9");
    v.edit_revert();
    assert_eq!(v.editing.as_deref(), Some("1"));
}

// ---- #663: entry shortcuts -------------------------------------------------------

fn abc_book() -> SheetView {
    let mut v = view();
    for r in 0..3 {
        put(&mut v, r, 0, Cell::number((r + 1) as f64));
    }
    put(&mut v, 0, 1, Cell::formula("A1*2"));
    v
}

#[test]
fn ctrl_enter_fills_the_selection_and_keeps_it() {
    let mut v = abc_book();
    v.sel = (0, 1);
    v.anchor = (2, 1);
    let undo = v.undo.len();
    v.begin_cell_edit(Some("=A1*10".into()));
    assert!(v.commit_edit_to_selection());
    assert!(v.editing.is_none());
    assert_eq!(v.undo.len(), undo + 1, "one undo step");
    assert_eq!((v.sel, v.anchor), ((0, 1), (2, 1)), "selection kept");
    for r in 0..3 {
        let cell = v.sheet().cell(r, 1).unwrap();
        assert_eq!(
            cell.formula.as_deref(),
            Some(format!("A{}*10", r + 1).as_str())
        );
        assert_eq!(cell.value, CellValue::Number(((r + 1) * 10) as f64));
    }
}

#[test]
fn ctrl_quote_copies_the_formula_above_and_ctrl_shift_quote_its_value() {
    let mut v = abc_book();
    select(&mut v, 1, 1);
    // What the key handler does: `sheet_begin_edit` opens the editor (bars
    // and fields give up the keyboard), then the chord fills it.
    assert!(v.entry_chord_applies("'"));
    v.begin_cell_edit(Some(String::new()));
    v.entry_chord("'", false, 0.0, true);
    assert_eq!(v.editing.as_deref(), Some("=A1*2"), "unadjusted");
    assert_eq!(v.edit_mode, EditMode::Edit);
    v.end_cell_edit();
    // Ctrl+Shift+" as the harness spells it, and as the platform does.
    for (key, shift) in [("\"", false), ("'", true)] {
        v.begin_cell_edit(Some(String::new()));
        v.entry_chord(key, shift, 0.0, true);
        assert_eq!(v.editing.as_deref(), Some("2"), "{key} {shift}");
        v.end_cell_edit();
    }
    // Row 1 has nothing above: the handler opens no editor for it.
    select(&mut v, 0, 1);
    assert!(!v.entry_chord_applies("'"));
    assert!(v.entry_chord_applies(";"));
    // Without an open editor a chord does nothing.
    select(&mut v, 1, 1);
    v.entry_chord("'", false, 0.0, false);
    assert!(v.editing.is_none());
    v.begin_cell_edit(Some(String::new()));
    v.entry_chord("\"", false, 0.0, true);
    assert_eq!(v.editing.as_deref(), Some("2"));
    assert!(v.commit_edit());
    assert_eq!(v.sheet().cell(1, 1).unwrap().formula, None);
    assert_eq!(value(&v, 1, 1), CellValue::Number(2.0));
}

#[test]
fn ctrl_d_and_ctrl_r_fill_with_moved_references_and_styles() {
    let mut v = abc_book();
    let bold = v.pkg.workbook.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    put(
        &mut v,
        0,
        1,
        Cell {
            style: bold,
            ..Cell::formula("A1*2")
        },
    );
    v.sel = (0, 1);
    v.anchor = (3, 1);
    let undo = v.undo.len();
    assert!(v.fill_selection(true));
    assert_eq!(v.undo.len(), undo + 1);
    assert_eq!(
        v.sheet().cell(2, 1).unwrap().formula.as_deref(),
        Some("A3*2")
    );
    assert_eq!(value(&v, 2, 1), CellValue::Number(6.0));
    assert!(xf(&v, 3, 1).bold);
    // D1:F1 with x in D1, Ctrl+R.
    put(&mut v, 0, 3, Cell::text("x"));
    v.sel = (0, 3);
    v.anchor = (0, 5);
    assert!(v.fill_selection(false));
    assert_eq!(value(&v, 0, 5), CellValue::Text("x".into()));
    // A single cell copies the cell above.
    select(&mut v, 1, 3);
    assert!(v.fill_selection(true));
    assert_eq!(value(&v, 1, 3), CellValue::Text("x".into()));
}

#[test]
fn ctrl_semicolon_enters_today_and_ctrl_shift_semicolon_the_time() {
    let mut v = view();
    // 2024-01-15 21:30.
    let now = 45306.0 + 21.5 / 24.0;
    v.begin_cell_edit(Some(String::new()));
    v.entry_chord(";", false, now, true);
    assert_eq!(v.editing.as_deref(), Some("1/15/2024"));
    assert_eq!(v.edit_mode, EditMode::Enter);
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Number(45306.0));
    assert_eq!(v.cell_text(0, 0), "1/15/2024");
    select(&mut v, 1, 0);
    v.begin_cell_edit(Some(String::new()));
    v.entry_chord(":", false, now, true);
    assert_eq!(v.editing.as_deref(), Some("9:30 PM"));
    // Into an editor already open, at the caret.
    v.editing = Some("at ".into());
    v.edit_caret_to_end();
    v.entry_chord(";", true, now, false);
    assert_eq!(v.editing.as_deref(), Some("at 9:30 PM"));
    v.editing = Some("9:30 PM".into());
    v.commit_edit();
    assert_eq!(v.cell_text(1, 0), "9:30 PM");
}

#[test]
fn f9_while_editing_replaces_the_formula_with_its_value() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::number(6.0));
    put(&mut v, 0, 1, Cell::formula("A1*7"));
    select(&mut v, 0, 1);
    v.begin_cell_edit(None);
    assert_eq!(v.editing.as_deref(), Some("=A1*7"));
    v.edit_eval_formula();
    assert_eq!(v.editing.as_deref(), Some("42"));
    assert!(v.commit_edit());
    let cell = v.sheet().cell(0, 1).unwrap();
    assert_eq!(cell.formula, None);
    assert_eq!(cell.value, CellValue::Number(42.0));
}

#[test]
fn f4_repeats_the_ribbons_last_toggle_as_a_setter() {
    let mut v = view();
    // Ctrl+B on A1 records what the ribbon's toggle resolved: bold ON.
    let bold = v.toggle_setter(|x| x.bold, |x, on| x.bold = on);
    v.apply_format(bold);
    assert!(xf(&v, 0, 0).bold);
    // F4 on E1, twice: it applies, never flips.
    select(&mut v, 0, 4);
    assert!(v.repeat_format());
    assert!(xf(&v, 0, 4).bold);
    assert!(v.repeat_format());
    assert!(xf(&v, 0, 4).bold);
    // The same toggle on a bold cell resolves to OFF.
    select(&mut v, 0, 0);
    let off = v.toggle_setter(|x| x.bold, |x, on| x.bold = on);
    v.apply_format(off);
    assert!(!xf(&v, 0, 0).bold);
    v.last_format = None;
    assert!(!v.repeat_format());
}

#[test]
fn a_refused_entry_stays_open_and_nothing_moves() {
    let mut v = view();
    // Sortable data in C1:C5 under the editor, so it is the guard, not an
    // empty sheet, that stops the sort.
    for (r, n) in [5.0, 3.0, 1.0, 4.0, 2.0].iter().enumerate() {
        put(&mut v, r as u32, 2, Cell::number(*n));
    }
    select(&mut v, 2, 2);
    v.begin_cell_edit(Some("y".repeat(32_768)));
    assert_eq!(v.commit_and_move(1, 0), None);
    let err = v.entry_error.clone().unwrap_or_default();
    assert!(err.contains("32767"), "{err}");
    assert_eq!(v.sel, (2, 2), "Enter/arrows do not move");
    assert!(v.editing.is_some(), "the editor stays open");
    // Inserting a row or sorting under it would move its origin: refused too.
    assert!(!v.structural_edit(StructOp::InsertRow));
    assert!(v.editing.is_some());
    assert_eq!(v.sort_with_pending_edit(None, &[(2, true)]), (false, false));
    assert!(v.undo.is_empty(), "nothing was done");
    assert_eq!(value(&v, 0, 2), CellValue::Number(5.0), "not sorted");
    // Shortened, it commits and moves.
    v.editing = Some("ok".into());
    assert_eq!(v.commit_and_move(1, 0), Some(true));
    assert_eq!(v.sel, (3, 2));
    assert_eq!(value(&v, 2, 2), CellValue::Text("ok".into()));
}

#[test]
fn a_number_format_choice_moves_the_classification_too() {
    // #654 via the ribbon: 50% then Comma, then 5 is 5 (not 0.05).
    let mut v = view();
    type_fresh(&mut v, "50%");
    v.commit_edit();
    v.apply_format(numfmt_setter("#,##0"));
    type_fresh(&mut v, "5");
    v.commit_edit();
    assert_eq!(value(&v, 0, 0), CellValue::Number(5.0));
    // A date set back to General: -1 shows -1, and a later entry is formatted.
    select(&mut v, 1, 0);
    type_fresh(&mut v, "1/15/2024");
    v.commit_edit();
    v.apply_format(numfmt_setter(""));
    type_fresh(&mut v, "-1");
    v.commit_edit();
    assert_eq!(v.cell_text(1, 0), "-1");
    type_fresh(&mut v, "1,234");
    v.commit_edit();
    assert_eq!(v.cell_text(1, 0), "1,234");
}

#[test]
fn replace_keeps_a_quote_prefixed_entry_text() {
    let mut v = view();
    for (r, text) in [(0, "'007"), (1, "'01234")] {
        select(&mut v, r, 0);
        type_fresh(&mut v, text);
        v.commit_edit();
    }
    assert!(v.replace_in_cell(0, 0, "0", "1"));
    assert_eq!(value(&v, 0, 0), CellValue::Text("117".into()));
    assert!(xf(&v, 0, 0).quote_prefix);
    assert!(v.replace_in_cell(1, 0, "4", "5"));
    assert_eq!(value(&v, 1, 0), CellValue::Text("01235".into()));
    // The prefix's `'` is not text: replacing `'` leaves 117 alone.
    assert!(!v.replace_in_cell(0, 0, "'", ""));
    assert_eq!(value(&v, 0, 0), CellValue::Text("117".into()));
    // A loaded 'abc (General, no prefix): its own `'` is text, matched once.
    put(&mut v, 5, 0, Cell::text("'abc"));
    select(&mut v, 4, 0);
    assert_eq!(v.find_match("''", false), None);
    assert!(v.replace_in_cell(5, 0, "'", "x"));
    assert_eq!(value(&v, 5, 0), CellValue::Text("xabc".into()));
    assert!(!xf(&v, 5, 0).quote_prefix, "the result has no ' of its own");
    // The apostrophe is decided from the result.
    put(&mut v, 6, 0, Cell::text("'5"));
    assert!(v.replace_in_cell(6, 0, "'", ""));
    assert_eq!(value(&v, 6, 0), CellValue::Number(5.0));
    assert!(!xf(&v, 6, 0).quote_prefix);
    put(&mut v, 7, 0, Cell::text("x'abc"));
    assert!(v.replace_in_cell(7, 0, "x", ""));
    assert_eq!(value(&v, 7, 0), CellValue::Text("'abc".into()));
    assert!(v.replace_in_cell(0, 0, "117", ""));
    assert_eq!(value(&v, 0, 0), CellValue::Empty, "an emptied text clears");
    // Any case, as the Find bar matches.
    put(&mut v, 2, 0, Cell::text("Alpha"));
    assert!(v.replace_in_cell(2, 0, "ALPHA", "Beta"));
    assert_eq!(value(&v, 2, 0), CellValue::Text("Beta".into()));
    assert!(!v.replace_in_cell(2, 0, "zzz", "q"));
    // A percent cell is not divided again: 150% with 5→6 is 160%.
    select(&mut v, 3, 0);
    type_fresh(&mut v, "150%");
    v.commit_edit();
    assert!(v.replace_in_cell(3, 0, "5", "6"));
    assert_eq!(value(&v, 3, 0), CellValue::Number(1.6));
    // Removing the % reads 160 as typed into a percent cell: 160%, not 16000%.
    assert!(v.replace_in_cell(3, 0, "%", ""));
    assert_eq!(value(&v, 3, 0), CellValue::Number(1.6));
    // A text beginning with ' (no prefix) keeps its apostrophe.
    put(&mut v, 4, 0, Cell::text("'abc"));
    assert!(v.replace_in_cell(4, 0, "abc", "xyz"));
    assert_eq!(value(&v, 4, 0), CellValue::Text("'xyz".into()));
}

#[test]
fn a_date_showing_hashes_is_not_found_or_replaced() {
    let mut v = view();
    let date = v.pkg.workbook.styles.intern(Xf {
        numfmt: gridcore::sheet::NumFmt::Date,
        code: Some("m/d/yyyy".into()),
        ..Xf::default()
    });
    put(
        &mut v,
        0,
        0,
        Cell {
            style: date,
            ..Cell::number(-1.0)
        },
    );
    assert_eq!(v.cell_text(0, 0), "########");
    assert_eq!(v.search_text(0, 0), "");
    assert_eq!(v.find_match("#", false), None);
    assert!(!v.replace_in_cell(0, 0, "#", ""));
    assert_eq!(value(&v, 0, 0), CellValue::Number(-1.0));
}

#[test]
fn find_and_replace_search_the_same_text() {
    let mut v = view();
    for (r, text) in ["1/15/2024", "50%", "'007", "=1+1", "plain"]
        .iter()
        .enumerate()
    {
        select(&mut v, r as u32, 0);
        type_fresh(&mut v, text);
        v.commit_edit();
    }
    // What each cell is searched as.
    assert_eq!(v.search_text(0, 0), "1/15/2024");
    assert_eq!(v.search_text(1, 0), "50%");
    assert_eq!(v.search_text(2, 0), "007", "no synthetic apostrophe");
    assert_eq!(v.search_text(3, 0), "=1+1");
    // Find selects what Replace then changes.
    select(&mut v, 4, 0);
    for (q, cell) in [
        ("2024", (0, 0)),
        ("50%", (1, 0)),
        ("007", (2, 0)),
        ("1+1", (3, 0)),
    ] {
        assert_eq!(v.find_match(q, false), Some(cell), "{q}");
    }
    // A formula's result is not its search text: from row 3 the scan passes
    // =1+1 (whose result is 2) and wraps round to the date's 2024.
    select(&mut v, 2, 0);
    assert_eq!(
        v.find_match("2", false),
        Some((0, 0)),
        "the date's 2024, not =1+1's 2"
    );
    // Replace inside the display of a date and a percent keeps working.
    assert!(v.replace_in_cell(0, 0, "2024", "2025"));
    assert_eq!(v.cell_text(0, 0), "1/15/2025");
    assert!(v.replace_in_cell(1, 0, "50", "60"));
    assert_eq!(value(&v, 1, 0), CellValue::Number(0.6));
    assert_eq!(v.cell_text(1, 0), "60%");
    // The formula's source is what is replaced.
    assert!(v.replace_in_cell(3, 0, "1+1", "2+2"));
    assert_eq!(
        v.sheet().cell(3, 0).unwrap().formula.as_deref(),
        Some("2+2")
    );
    assert_eq!(value(&v, 3, 0), CellValue::Number(4.0));
}
