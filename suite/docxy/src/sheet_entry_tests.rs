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
fn enter_mode_arrows_commit_except_while_a_formula_is_typed() {
    let mut v = view();
    type_fresh(&mut v, "abc");
    assert!(!v.edit_arrows_move_caret());
    for formula in ["=A1+", "-L1", "+A99", "@SUM(1"] {
        v.editing = Some(formula.into());
        let want = formula != "@SUM(1";
        assert_eq!(v.edit_arrows_move_caret(), want, "{formula}");
    }
    v.editing = Some("-5".into());
    assert!(!v.edit_arrows_move_caret(), "a number is not a formula");
    v.edit_mode = EditMode::Edit;
    v.editing = Some("abc".into());
    assert!(v.edit_arrows_move_caret());
    // F2 / a double-click / the fx bar open in Edit mode.
    v.end_cell_edit();
    v.begin_cell_edit(None);
    assert_eq!(v.edit_mode, EditMode::Edit);
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
    v.edit_copy_from_above(false);
    assert_eq!(v.editing.as_deref(), Some("=A1*2"), "unadjusted");
    v.end_cell_edit();
    v.edit_copy_from_above(true);
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
    v.edit_insert_now(false, now);
    assert_eq!(v.editing.as_deref(), Some("1/15/2024"));
    assert_eq!(v.edit_mode, EditMode::Enter);
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Number(45306.0));
    assert_eq!(v.cell_text(0, 0), "1/15/2024");
    select(&mut v, 1, 0);
    v.edit_insert_now(true, now);
    assert_eq!(v.editing.as_deref(), Some("9:30 PM"));
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
fn f4_repeats_the_last_format_as_a_setter() {
    let mut v = view();
    // What `sheet_format` records for Ctrl+B on a plain cell: bold ON.
    let on = true;
    v.last_format = Some(std::rc::Rc::new(move |xf: &mut Xf| xf.bold = on));
    select(&mut v, 0, 4);
    assert!(v.repeat_format());
    assert!(xf(&v, 0, 4).bold);
    // Repeating again applies, it does not flip.
    assert!(v.repeat_format());
    assert!(xf(&v, 0, 4).bold);
    assert_eq!(v.undo.len(), 2);
    v.last_format = None;
    assert!(!v.repeat_format());
}
