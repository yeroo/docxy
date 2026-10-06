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
fn the_editor_seeds_every_digit_and_a_percent_cell_its_percent() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::number(0.1 + 0.2));
    assert_eq!(v.edit_string(0, 0), "0.30000000000000004");
    let pct = v.pkg.workbook.styles.intern({
        let mut xf = Xf::default();
        xf.set_code(Some("0%".into()));
        xf
    });
    put(
        &mut v,
        1,
        0,
        Cell {
            style: pct,
            ..Cell::number(1.5)
        },
    );
    assert_eq!(v.edit_string(1, 0), "150%");
    // F2 then Enter unchanged leaves it; edited to 160% it is 1.6 (a plain
    // `1.5` seed edited to `1.6` used to be divided again, to 0.016).
    select(&mut v, 1, 0);
    v.begin_cell_edit(None);
    assert_eq!(v.editing.as_deref(), Some("150%"));
    v.commit_edit();
    assert_eq!(value(&v, 1, 0), CellValue::Number(1.5));
    v.begin_cell_edit(None);
    v.editing = Some("160%".into());
    assert!(v.commit_edit());
    assert_eq!(value(&v, 1, 0), CellValue::Number(1.6));
}

#[test]
fn a_general_number_is_fitted_to_its_cell() {
    let general = Xf::default();
    let big = CellValue::Number(123_456_789_012.0);
    // A default column (col_px of 8.43) and a wide one: General stops at 11
    // characters either way, shorter when the cell is narrower.
    let default_w = col_px(8.43);
    let chars = (((default_w - 6.0) / 7.0).floor() as usize).max(1);
    assert_eq!(
        grid_cell_text(&general, &big, false, default_w),
        gridcore::sheet::fmt_general_cell(123_456_789_012.0, chars)
    );
    assert_ne!(
        grid_cell_text(&general, &big, false, default_w),
        "123456789012"
    );
    assert_eq!(grid_cell_text(&general, &big, false, 200.0), "1.23457E+11");
    assert_eq!(
        grid_cell_text(&general, &CellValue::Number(42.0), false, 64.0),
        "42"
    );
    let mut fixed = Xf::default();
    fixed.set_code(Some("0.00".into()));
    assert_eq!(
        grid_cell_text(&fixed, &CellValue::Number(1.5), false, 64.0),
        "1.50"
    );
}

#[test]
fn a_grid_clip_pastes_the_same_cells_between_text_and_general() {
    // The grid clip carries the cells, so a quote-prefixed General `007`
    // onto a Text cell, and a Text cell's `'abc` onto a General one, land as
    // they were: no apostrophe added or dropped.
    let mut v = view();
    let quoted = v.pkg.workbook.styles.intern(Xf {
        quote_prefix: true,
        ..Xf::default()
    });
    let text_fmt = v.pkg.workbook.styles.intern({
        let mut xf = Xf::default();
        xf.set_code(Some("@".into()));
        xf
    });
    let q007 = Cell {
        style: quoted,
        ..Cell::text("007")
    };
    let tabc = Cell {
        style: text_fmt,
        ..Cell::text("'abc")
    };
    put(&mut v, 0, 0, q007.clone());
    put(&mut v, 0, 1, tabc.clone());
    put(
        &mut v,
        2,
        0,
        Cell {
            style: text_fmt,
            ..Cell::default()
        },
    );
    put(&mut v, 2, 1, Cell::text("x"));
    let block = vec![vec![q007.clone(), tabc.clone()]];
    let s = v.active;
    v.engine.paste_block(&mut v.pkg.workbook, s, (2, 0), &block);
    assert_eq!(v.sheet().cell(2, 0), Some(&q007));
    assert_eq!(v.sheet().cell(2, 1), Some(&tabc));
}

#[test]
fn the_editor_shows_one_line_per_line_feed_with_the_caret_on_its_line() {
    let text = |line: &EditLine| line.iter().map(|(_, s, _)| s.as_str()).collect::<String>();
    let offs = |line: &EditLine| line.iter().map(|(o, _, _)| *o).collect::<Vec<_>>();
    // "ab\ncd": the caret (after `c`, char 4) is on the second line.
    let lines = edit_lines("ab\ncd", 4);
    assert_eq!(lines.len(), 2);
    assert_eq!(text(&lines[0].0), "ab");
    assert_eq!(text(&lines[1].0), "cd");
    assert_eq!((lines[0].1, lines[1].1), (false, true));
    // Offsets are into the whole buffer, split at the caret too.
    assert_eq!(offs(&lines[1].0), vec![3, 4]);
    // A caret right after a line feed is on the next line; right before it,
    // on the line it ends.
    assert!(edit_lines("ab\ncd", 3)[1].1);
    assert!(edit_lines("ab\ncd", 2)[0].1);
    // Empty lines are kept: a trailing Alt+Enter opens a line to type on.
    let lines = edit_lines("a\n\n", 4);
    assert_eq!(lines.len(), 3);
    assert!(lines[1].0.is_empty() && lines[2].0.is_empty());
    assert!(lines[2].1);
    // One line without a feed; a formula keeps its reference colours.
    let lines = edit_lines("=A1+\nB2", 0);
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0]
            .0
            .iter()
            .any(|(_, s, ci)| s == "A1" && ci.is_some())
    );
    assert!(
        lines[1]
            .0
            .iter()
            .any(|(_, s, ci)| s == "B2" && ci.is_some())
    );
    assert_eq!(edit_lines("abc", 1).len(), 1);
}

#[test]
fn ctrl_enter_checks_the_formula_in_every_cell_of_the_range() {
    // A1 is Text (it would keep `=SUM(B1` as text), A2:A3 General: the
    // formula they would get does not parse, so nothing is entered.
    let mut v = view();
    let text_fmt = v.pkg.workbook.styles.intern({
        let mut xf = Xf::default();
        xf.set_code(Some("@".into()));
        xf
    });
    put(
        &mut v,
        0,
        0,
        Cell {
            style: text_fmt,
            ..Cell::default()
        },
    );
    select(&mut v, 0, 0);
    v.anchor = (2, 0);
    type_fresh(&mut v, "=SUM(B1");
    assert!(!v.commit_edit_to_selection());
    assert!(
        v.entry_error
            .as_deref()
            .is_some_and(|e| e.starts_with("formula error"))
    );
    assert_eq!(v.editing.as_deref(), Some("=SUM(B1"));
    for r in 0..3 {
        assert!(v.sheet().cell(r, 0).is_none_or(|c| c.is_blank()), "row {r}");
    }
    // Finished, it commits: text in A1, formulas below.
    v.editing = Some("=SUM(B1)".into());
    v.entry_error = None;
    assert!(v.commit_edit_to_selection());
    assert_eq!(value(&v, 0, 0), CellValue::Text("=SUM(B1)".into()));
    let f = |r| v.sheet().cell(r, 0).and_then(|c| c.formula.clone());
    assert_eq!(f(1).as_deref(), Some("SUM(B2)"));
    assert_eq!(f(2).as_deref(), Some("SUM(B3)"));
}

#[test]
fn an_unfinished_formula_is_refused_and_the_editor_stays() {
    let mut v = view();
    type_fresh(&mut v, "=SUM(A1");
    // Right in Enter mode after a reference is a commit, not a point...
    assert!(!v.edit_point(0, 1, false));
    // ...and the commit is refused: nothing stored, nothing moved.
    assert_eq!(v.commit_and_move(0, 1), None);
    assert_eq!(v.editing.as_deref(), Some("=SUM(A1"));
    assert!(
        v.entry_error
            .as_deref()
            .is_some_and(|e| e.starts_with("formula error"))
    );
    assert_eq!(v.sel, (0, 0));
    assert!(v.sheet().cell(0, 0).is_none());
    // Enter the same.
    v.entry_error = None;
    assert!(!v.commit_edit());
    assert!(v.entry_error.is_some());
    assert!(v.sheet().cell(0, 0).is_none());
    // Finished, it commits.
    v.editing = Some("=SUM(A1)".into());
    v.entry_error = None;
    assert!(v.commit_edit());
    assert_eq!(
        v.sheet().cell(0, 0).and_then(|c| c.formula.as_deref()),
        Some("SUM(A1)")
    );
    // A Text cell stores `=SUM(A1` as text: nothing to refuse there.
    let text_fmt = v.pkg.workbook.styles.intern({
        let mut xf = Xf::default();
        xf.set_code(Some("@".into()));
        xf
    });
    put(
        &mut v,
        1,
        0,
        Cell {
            style: text_fmt,
            ..Cell::default()
        },
    );
    select(&mut v, 1, 0);
    type_fresh(&mut v, "=SUM(A1");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 1, 0), CellValue::Text("=SUM(A1".into()));
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
    assert!(v.fill_selection(gridcore::edit::FillDir::Down));
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
    assert!(v.fill_selection(gridcore::edit::FillDir::Right));
    assert_eq!(value(&v, 0, 5), CellValue::Text("x".into()));
    // A single cell copies the cell above.
    select(&mut v, 1, 3);
    assert!(v.fill_selection(gridcore::edit::FillDir::Down));
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
    assert!(crate::sheet_sort::sort_view(&mut v, (0, 2, 4, 2), &by(2), &no_header()).is_err());
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

#[test]
fn a_fill_mixing_a_blank_into_a_frozen_block_clears_it() {
    // #840 (r4-m1): Ctrl+R over D2:E3 from D2 = blank, D3 = 5 into a frozen
    // array block E1:E3 (an anchor the engine can't evaluate, cached
    // 7/8/9): the 5 breaks the block, and the blank clears E2 though it
    // comes first.
    let mut v = view();
    let s = v.active;
    let sheet = &mut v.pkg.workbook.sheets[s];
    sheet.set_cell(0, 0, Cell::number(1.0));
    sheet.set_cell(
        0,
        4,
        Cell {
            value: CellValue::Number(7.0),
            formula: Some("PIVOTBY(A1,4)".into()),
            f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
            spill: Some((3, 1)),
            ..Cell::default()
        },
    );
    sheet.set_cell(1, 4, Cell::number(8.0));
    sheet.set_cell(2, 4, Cell::number(9.0));
    sheet.set_cell(2, 3, Cell::number(5.0));
    v.engine = sheet_engine(&v.pkg.workbook);
    v.sel = (1, 3);
    v.anchor = (2, 4);
    assert!(v.fill_selection(gridcore::edit::FillDir::Right));
    assert_eq!(value(&v, 0, 4), CellValue::Number(7.0));
    assert_eq!(value(&v, 1, 4), CellValue::Empty);
    assert_eq!(value(&v, 2, 4), CellValue::Number(5.0));
}

#[test]
fn a_sort_across_a_spill_is_refused_with_a_reason() {
    // #840: rows that cut a spilled array don't sort; `entry_error` (shown
    // in the tab's status) says why, and no undo step is pushed.
    let mut v = view();
    for (r, n) in [3.0, 1.0, 2.0].iter().enumerate() {
        put(&mut v, r as u32, 0, Cell::number(*n));
    }
    put(&mut v, 0, 2, Cell::formula("SEQUENCE(3)"));
    assert_eq!(value(&v, 2, 2), CellValue::Number(3.0));
    select(&mut v, 0, 0);
    let undo = v.undo.len();
    // The list A1:C3 holds the spill.
    assert_eq!(
        crate::sheet_sort::sort_view(&mut v, (0, 0, 2, 2), &by(0), &no_header()),
        Err(gridcore::edit::SORT_CUTS_SPILL.to_string())
    );
    assert_eq!(
        v.entry_error.as_deref(),
        Some(gridcore::edit::SORT_CUTS_SPILL)
    );
    assert_eq!(v.undo.len(), undo);
    assert_eq!(value(&v, 0, 0), CellValue::Number(3.0));
}

#[test]
fn a_replace_all_mixing_a_blank_into_a_frozen_block_clears_it() {
    // #840 r1 p1: replace "x" with "" over a frozen array block E1:E3 whose
    // cached cells are E2 "x", E3 "xy": E2 becomes blank and E3 "y" in one
    // group, so the blank clears E2 though it comes first.
    let mut v = view();
    let s = v.active;
    let sheet = &mut v.pkg.workbook.sheets[s];
    sheet.set_cell(0, 0, Cell::number(1.0));
    sheet.set_cell(
        0,
        4,
        Cell {
            value: CellValue::Number(7.0),
            formula: Some("PIVOTBY(A1,4)".into()),
            f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
            spill: Some((3, 1)),
            ..Cell::default()
        },
    );
    sheet.set_cell(1, 4, Cell::text("x"));
    sheet.set_cell(2, 4, Cell::text("xy"));
    v.engine = sheet_engine(&v.pkg.workbook);
    assert_eq!(v.replace_all_cells("x", ""), 2);
    assert_eq!(value(&v, 0, 4), CellValue::Number(7.0));
    assert_eq!(value(&v, 1, 4), CellValue::Empty);
    assert_eq!(value(&v, 2, 4), CellValue::Text("y".into()));
}

// ---- #775 r7: an edit to part of a legacy CSE array is refused whole ------

/// A1:A3 = 1, 2, 3 and a legacy CSE block `{=A1:A3*2}` over D1:D3 = 2, 4, 6.
fn view_with_cse_block() -> SheetView {
    let mut v = view();
    let s = v.active;
    let sheet = &mut v.pkg.workbook.sheets[s];
    for r in 0..3u32 {
        sheet.set_cell(r, 0, Cell::number(f64::from(r + 1)));
    }
    let mut d1 = Cell::formula("A1:A3*2");
    d1.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".into());
    sheet.set_cell(0, 3, d1);
    v.engine = sheet_engine(&v.pkg.workbook);
    v.engine.recalc_all(&mut v.pkg.workbook);
    assert_eq!(value(&v, 2, 3), CellValue::Number(6.0));
    v
}

/// Nothing changed, no undo step, and Excel's reason to show.
fn assert_refused(v: &mut SheetView, before: &std::collections::BTreeMap<(u32, u32), Cell>) {
    assert_eq!(&v.sheet().cells, before);
    assert!(v.undo.is_empty());
    assert_eq!(
        v.entry_error.take().as_deref(),
        Some(gridcore::engine::PART_OF_ARRAY)
    );
}

#[test]
fn typing_into_part_of_an_array_is_refused_and_keeps_the_editor() {
    let mut v = view_with_cse_block();
    let before = v.sheet().cells.clone();
    select(&mut v, 1, 3);
    type_fresh(&mut v, "9");
    assert_eq!(v.commit_and_move(1, 0), None);
    assert_eq!(v.editing.as_deref(), Some("9"));
    assert_eq!(v.sel, (1, 3));
    assert_refused(&mut v, &before);
}

#[test]
fn a_range_entry_or_fill_over_part_of_an_array_is_refused_whole() {
    // Ctrl+Enter into D2:E3 would write E2:E3 too; none of it lands.
    let mut v = view_with_cse_block();
    let before = v.sheet().cells.clone();
    v.anchor = (1, 3);
    v.sel = (2, 4);
    type_fresh(&mut v, "5");
    v.anchor = (1, 3);
    v.sel = (2, 4);
    assert!(!v.commit_edit_to_selection());
    assert_refused(&mut v, &before);
    v.end_cell_edit();
    // Ctrl+D over C2:D4 from C2 = 5, D2 = 4.
    put(&mut v, 1, 2, Cell::number(5.0));
    let before = v.sheet().cells.clone();
    v.anchor = (1, 2);
    v.sel = (3, 3);
    assert!(!v.fill_selection(gridcore::edit::FillDir::Down));
    assert_refused(&mut v, &before);
}

#[test]
fn clearing_part_of_an_array_is_refused_but_not_all_of_it() {
    // What Delete and a cut ask before they snapshot.
    let mut v = view_with_cse_block();
    let s = v.active;
    v.anchor = (1, 3);
    v.sel = (2, 3);
    let part = v.clear_changes();
    assert!(v.refuses(s, &part));
    v.entry_error = None;
    v.anchor = (0, 3);
    let all = v.clear_changes();
    assert!(!v.refuses(s, &all));
    assert!(v.entry_error.is_none());
}

#[test]
fn a_drag_fill_from_a_source_holding_the_anchor_is_refused() {
    // r8 M2: D1 = 5 and a block anchored at D2 over D2:D4. D1:D2 dragged
    // down to D6 writes D3:D6 only: the anchor stays in the source, so the
    // fill writes into part of the block and is refused.
    let mut v = view();
    let s = v.active;
    let sheet = &mut v.pkg.workbook.sheets[s];
    sheet.set_cell(0, 0, Cell::number(1.0));
    sheet.set_cell(0, 3, Cell::number(5.0));
    let mut d2 = Cell::formula("A1*2");
    d2.f_attrs = Some(" t=\"array\" ref=\"D2:D4\"".into());
    sheet.set_cell(1, 3, d2);
    v.engine = sheet_engine(&v.pkg.workbook);
    v.engine.recalc_all(&mut v.pkg.workbook);
    let src = (0, 3, 1, 3);
    let bx = fill_box(src, (5, 3));
    assert_eq!(bx, (0, 3, 5, 3));
    let gridcore::edit::FillTarget::Extend { dest, .. } = gridcore::edit::fill_target(src, (5, 3))
    else {
        panic!("a fill down")
    };
    assert_eq!(dest, (2, 3, 5, 3));
    assert!(v.engine.refuses_area(&v.pkg.workbook, s, dest));
    // The whole box would count the anchor as replaced: the r7 bug.
    assert!(!v.engine.refuses_area(&v.pkg.workbook, s, bx));
    // The drag itself: refused whole, nothing written, no undo step.
    let before = v.sheet().cells.clone();
    assert_eq!(
        v.fill_drag(&gridcore::edit::FillReq {
            src,
            to: (5, 3),
            kind: gridcore::edit::FillKind::Auto,
            ctrl: false,
            lists: &[],
        }),
        Err(gridcore::engine::PART_OF_ARRAY.to_string())
    );
    assert_eq!(v.sheet().cells, before);
    assert!(v.undo.is_empty());
    // Right: the columns past the source.
    assert!(matches!(
        gridcore::edit::fill_target((0, 0, 1, 1), (1, 4)),
        gridcore::edit::FillTarget::Extend {
            dest: (0, 2, 1, 4),
            ..
        }
    ));
}

// ---- #672: Excel's editing options ----------------------------------------

use gridcore::options::{EditOptions, EnterMove};

fn opts_view(opts: EditOptions) -> SheetView {
    let mut v = view();
    v.edit_opts = opts;
    v
}

fn text(v: &mut SheetView, r: u32, c: u32, t: &str) {
    put(v, r, c, Cell::text(t));
}

#[test]
fn a_typed_commit_takes_the_fixed_decimal() {
    let mut v = opts_view(EditOptions {
        fixed_decimal: true,
        ..EditOptions::default()
    });
    type_fresh(&mut v, "1234");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 0, 0), CellValue::Number(12.34));
    select(&mut v, 1, 0);
    type_fresh(&mut v, "1.");
    assert!(v.commit_edit());
    assert_eq!(
        value(&v, 1, 0),
        CellValue::Number(1.0),
        "a typed point wins"
    );
    // Ctrl+Enter over A3:A4 shifts each cell.
    v.sel = (2, 0);
    v.anchor = (3, 0);
    v.begin_cell_edit(Some(String::new()));
    v.edit_insert("5");
    assert!(v.commit_edit_to_selection());
    assert_eq!(value(&v, 2, 0), CellValue::Number(0.05));
    assert_eq!(value(&v, 3, 0), CellValue::Number(0.05));
    // An untouched F2 edit of an integer is not re-read; a changed one is.
    select(&mut v, 5, 0);
    put(&mut v, 5, 0, Cell::number(1234.0));
    v.begin_cell_edit(None);
    assert!(!v.commit_edit());
    assert_eq!(value(&v, 5, 0), CellValue::Number(1234.0));
    v.begin_cell_edit(None);
    v.edit_caret_to_end();
    v.edit_insert("5");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 5, 0), CellValue::Number(123.45));
    // Off: as typed.
    v.edit_opts.fixed_decimal = false;
    select(&mut v, 6, 0);
    type_fresh(&mut v, "1234");
    assert!(v.commit_edit());
    assert_eq!(value(&v, 6, 0), CellValue::Number(1234.0));
}

#[test]
fn enter_moves_the_way_the_options_say() {
    for (dir, want) in [
        (EnterMove::Down, (3, 2)),
        (EnterMove::Right, (2, 3)),
        (EnterMove::Up, (1, 2)),
        (EnterMove::Left, (2, 1)),
    ] {
        let mut v = opts_view(EditOptions {
            enter_move: dir,
            ..EditOptions::default()
        });
        select(&mut v, 2, 2);
        type_fresh(&mut v, "x");
        let (dr, dc) = v.enter_delta(false);
        assert_eq!(v.commit_and_move(dr, dc), Some(true));
        assert_eq!(v.sel, want, "{dir:?}");
        let (dr, dc) = v.enter_delta(true);
        v.commit_and_move(dr, dc);
        assert_eq!(v.sel, (2, 2), "{dir:?}: Shift+Enter goes back");
    }
    let mut v = opts_view(EditOptions {
        move_after_enter: false,
        ..EditOptions::default()
    });
    select(&mut v, 2, 2);
    type_fresh(&mut v, "y");
    assert_eq!(v.enter_delta(false), (0, 0));
    assert_eq!(v.enter_delta(true), (0, 0));
    assert_eq!(v.commit_and_move(0, 0), Some(true));
    assert_eq!(
        (v.sel, v.editing.is_none()),
        ((2, 2), true),
        "commits and stays"
    );
    assert_eq!(value(&v, 2, 2), CellValue::Text("y".into()));
}

fn type_chars(v: &mut SheetView, t: &str) {
    for ch in t.chars() {
        v.type_char(&ch.to_string());
    }
}

#[test]
fn autocomplete_proposes_and_any_commit_takes_it() {
    let mut v = view();
    text(&mut v, 0, 0, "Apple");
    text(&mut v, 1, 0, "Banana");
    select(&mut v, 2, 0);
    type_chars(&mut v, "AP");
    assert_eq!(v.editing.as_deref(), Some("APple"));
    assert_eq!(
        (v.edit_caret, v.edit_proposal.clone()),
        (2, Some((2, "Apple".into())))
    );
    assert!(v.commit_edit());
    assert_eq!(value(&v, 2, 0), CellValue::Text("Apple".into()), "its case");
    // Backspace (or Delete) drops only the suffix; a typed char matches again.
    select(&mut v, 3, 0);
    type_chars(&mut v, "b");
    assert_eq!(v.editing.as_deref(), Some("banana"));
    assert!(v.drop_proposal());
    assert_eq!((v.editing.as_deref(), v.edit_caret), (Some("b"), 1));
    type_chars(&mut v, "a");
    assert_eq!(v.editing.as_deref(), Some("banana"));
    type_chars(&mut v, "x");
    assert_eq!(v.editing.as_deref(), Some("bax"), "x replaced the suffix");
    v.end_cell_edit();
    // A commit by moving away (a click, an arrow) takes it too.
    select(&mut v, 3, 0);
    type_chars(&mut v, "ban");
    assert_eq!(v.commit_and_move(0, 1), Some(true));
    assert_eq!(value(&v, 3, 0), CellValue::Text("Banana".into()));
    // Ctrl+Enter takes it into every cell.
    v.sel = (4, 0);
    v.anchor = (5, 0);
    type_chars(&mut v, "ap");
    assert!(v.commit_edit_to_selection());
    assert_eq!(value(&v, 5, 0), CellValue::Text("Apple".into()));
    // A caret move drops the marker: the text stays as shown.
    select(&mut v, 7, 0);
    put(&mut v, 6, 0, Cell::text("Cherry"));
    type_chars(&mut v, "c");
    v.edit_proposal = None;
    assert!(v.commit_edit());
    assert_eq!(value(&v, 7, 0), CellValue::Text("cherry".into()));
    // Off: nothing is proposed.
    v.edit_opts.autocomplete = false;
    select(&mut v, 8, 0);
    type_chars(&mut v, "ch");
    assert_eq!(v.editing.as_deref(), Some("ch"));
}

#[test]
fn autocomplete_waits_for_the_caret_at_the_end() {
    let mut v = view();
    text(&mut v, 0, 0, "Apple");
    select(&mut v, 1, 0);
    type_chars(&mut v, "p");
    v.edit_caret = 0;
    type_chars(&mut v, "a");
    assert_eq!(v.editing.as_deref(), Some("ap"), "typed before the end");
}

#[test]
fn a_double_click_jumps_to_the_precedent_with_editing_in_cells_off() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::formula("SUM(C3:D4)+B1"));
    put(&mut v, 0, 1, Cell::formula("1+2"));
    put(&mut v, 0, 5, Cell::number(7.0));
    assert!(!v.double_click_jumps(0, 0), "on: a double-click edits");
    v.edit_opts.edit_in_cell = false;
    assert!(v.double_click_jumps(0, 0));
    assert!(!v.double_click_jumps(0, 5), "a constant still edits");
    assert_eq!(v.goto_precedent(0, 0), Ok(false));
    assert_eq!((v.sel, v.anchor), ((2, 2), (3, 3)));
    assert!(v.goto_precedent(0, 1).is_err(), "no precedent");
    assert_eq!(v.sel, (2, 2), "nothing moved");
}

#[test]
fn a_precedent_on_another_sheet_switches_to_it_unless_hidden() {
    let mut v = view();
    v.pkg.workbook.sheets.push(gridcore::sheet::Sheet {
        name: "Data".into(),
        ..Default::default()
    });
    put(&mut v, 0, 0, Cell::formula("Data!B2*2"));
    v.edit_opts.edit_in_cell = false;
    v.pkg.workbook.sheets[1].hidden = true;
    assert!(v.goto_precedent(0, 0).is_err());
    assert_eq!(v.active, 0);
    v.pkg.workbook.sheets[1].hidden = false;
    assert_eq!(v.goto_precedent(0, 0), Ok(true));
    assert_eq!((v.active, v.sel, v.anchor), (1, (1, 1), (1, 1)));
}

#[test]
fn the_fill_handle_option_hides_and_disarms_the_handle() {
    let mut fill = None;
    assert!(!arm_fill(
        &mut fill,
        Some((0, 0, 0, 0)),
        false,
        false,
        false,
        false
    ));
    assert!(fill.is_none());
    assert_eq!(
        fill_handle_hidden(false, false, false, false, None),
        Some(HandleHidden::Disabled)
    );
    assert!(!HandleHidden::Disabled.cleared_by_a_click());
    assert!(arm_fill(
        &mut fill,
        Some((0, 0, 0, 0)),
        false,
        false,
        false,
        true
    ));
}

#[test]
fn editing_in_cells_off_draws_the_entry_as_plain_text() {
    assert_eq!(cell_edit_draw(true, true, true), CellEditDraw::Editor);
    assert_eq!(cell_edit_draw(true, true, false), CellEditDraw::Plain);
    assert_eq!(cell_edit_draw(false, true, false), CellEditDraw::None);
    assert_eq!(cell_edit_draw(true, false, true), CellEditDraw::None);
}

#[test]
fn the_status_bar_says_fixed_decimal_while_it_is_on() {
    let mut o = EditOptions::default();
    assert!(sheet_status_words(&o).is_empty());
    o.fixed_decimal = true;
    assert_eq!(sheet_status_words(&o), ["Fixed Decimal"]);
}

#[test]
fn the_formula_bar_expands_to_about_four_lines() {
    assert_eq!(fx_bar_height(false), 26.);
    assert!(fx_bar_height(true) >= 4. * 16.);
}

#[test]
fn settings_direction_cycles_every_way() {
    let mut m = EnterMove::Down;
    let mut seen = vec![m];
    for _ in 0..3 {
        m = next_enter_move(m);
        seen.push(m);
    }
    assert_eq!(seen, EnterMove::ALL);
    assert_eq!(next_enter_move(EnterMove::Left), EnterMove::Down);
}

#[test]
fn an_inserting_chord_drops_the_proposal_suffix_first() {
    let mut v = view();
    text(&mut v, 0, 0, "Apple");
    select(&mut v, 1, 0);
    // Alt+Enter: the line feed lands after the typed text.
    type_chars(&mut v, "ap");
    v.proposal_before_key("enter", false, true);
    v.edit_type("\n");
    assert_eq!(v.editing.as_deref(), Some("ap\n"));
    assert_eq!(v.edit_proposal, None);
    v.end_cell_edit();
    // Ctrl+; : today's date lands after the typed text.
    type_chars(&mut v, "ap");
    v.proposal_before_key(";", true, false);
    v.entry_chord(";", false, 45_565.0, false);
    let buf = v.editing.clone().unwrap();
    assert!(buf.starts_with("ap") && !buf.contains("ple"), "{buf}");
    v.end_cell_edit();
    // A caret move keeps the text and drops only the marker.
    type_chars(&mut v, "ap");
    v.proposal_before_key("home", false, false);
    assert_eq!(
        (v.editing.as_deref(), v.edit_proposal.clone()),
        (Some("apple"), None)
    );
    v.end_cell_edit();
    // Ctrl+Enter commits: the proposal stays for the commit to take.
    type_chars(&mut v, "ap");
    v.proposal_before_key("enter", true, false);
    assert!(v.edit_proposal.is_some());
    // So does Ctrl+S: saving commits the open editor, which takes it.
    v.proposal_before_key("s", true, false);
    assert!(v.edit_proposal.is_some());
    assert!(v.commit_edit());
    assert_eq!(value(&v, 1, 0), CellValue::Text("Apple".into()));
    // Ctrl+Left moves the caret by a word: the marker goes.
    select(&mut v, 2, 0);
    type_chars(&mut v, "ap");
    v.proposal_before_key("left", true, false);
    assert_eq!(v.edit_proposal, None);
}

#[test]
fn ctrl_shift_u_is_the_formula_bar_key_even_in_protected_view() {
    // `sheet_key` asks this before Protected View's gate, which would
    // refuse a Ctrl chord other than c/a/f.
    assert!(fx_toggle_key(true, true, "u"));
    assert!(fx_toggle_key(true, true, "U"));
    assert!(!fx_toggle_key(true, false, "u"), "Ctrl+U alone is not it");
    assert!(!open_mode::protected_allows_key("u", true, false));
}

/// One A to Z level on column `col`.
fn by(col: u32) -> [gridcore::edit::SortLevel; 1] {
    [gridcore::edit::SortLevel {
        key: col,
        on: gridcore::edit::SortOn::Value {
            asc: true,
            list: None,
        },
    }]
}

fn no_header() -> gridcore::edit::SortOptions {
    gridcore::edit::SortOptions::default()
}

// ---- #682: typing beside a table grows it; a formula fills its column ------

/// The issue's `Calc` table on A1:C4 (Qty 2, 3, 4; Price 5, 6, 7; Line
/// empty) with `=SUM(Calc[Qty])` in E1.
fn calc_view() -> SheetView {
    let mut v = view();
    for (c, h) in ["Qty", "Price", "Line"].iter().enumerate() {
        put(&mut v, 0, c as u32, Cell::text(h));
    }
    for (i, (q, p)) in [(2.0, 5.0), (3.0, 6.0), (4.0, 7.0)].into_iter().enumerate() {
        put(&mut v, i as u32 + 1, 0, Cell::number(q));
        put(&mut v, i as u32 + 1, 1, Cell::number(p));
    }
    v.pkg
        .add_table(0, (0, 0, 3, 2), true, "TableStyleMedium2")
        .unwrap();
    gridcore::edit::rename_table(&mut v.pkg.workbook, "Table1", "Calc").unwrap();
    v.engine = sheet_engine(&v.pkg.workbook);
    put(&mut v, 0, 4, Cell::formula("SUM(Calc[Qty])"));
    v
}

fn type_at(v: &mut SheetView, r: u32, c: u32, text: &str) {
    select(v, r, c);
    type_fresh(v, text);
    assert!(v.commit_edit(), "{text}");
}

#[test]
fn typed_entries_grow_tables_and_fill_calculated_columns() {
    let mut v = calc_view();
    type_at(&mut v, 1, 2, "=[@Qty]*[@Price]");
    assert_eq!(value(&v, 3, 2), CellValue::Number(28.0));
    type_at(&mut v, 4, 0, "5");
    assert_eq!(v.pkg.workbook.tables[0].range, (0, 0, 4, 2));
    assert_eq!(value(&v, 0, 4), CellValue::Number(14.0));
    type_at(&mut v, 4, 1, "2");
    assert_eq!(value(&v, 4, 2), CellValue::Number(10.0));
    // B5's entry, then A5's expansion: the table is back, A5 stays.
    assert!(v.undo_step());
    assert!(v.undo_step());
    assert_eq!(v.pkg.workbook.tables[0].range, (0, 0, 3, 2));
    assert_eq!(value(&v, 4, 0), CellValue::Number(5.0));
    assert!(v.redo_step());
    assert_eq!(v.pkg.workbook.tables[0].range, (0, 0, 4, 2));
}

#[test]
fn the_autocorrect_switches_turn_the_table_rules_off() {
    let mut v = calc_view();
    let mut ac = (*v.autocorrect).clone();
    ac.opts.table_rows_cols = false;
    v.autocorrect = std::rc::Rc::new(ac);
    type_at(&mut v, 4, 0, "5");
    assert_eq!(v.pkg.workbook.tables[0].range, (0, 0, 3, 2));

    let mut v = calc_view();
    let mut ac = (*v.autocorrect).clone();
    ac.opts.table_formulas = false;
    v.autocorrect = std::rc::Rc::new(ac);
    type_at(&mut v, 1, 2, "=[@Qty]*[@Price]");
    assert_eq!(value(&v, 2, 2), CellValue::Empty);
    assert!(v.pkg.workbook.tables[0].calculated_formulas.is_empty());
}

// ---- #510: an act that reads or rewrites cell content commits the open editor

/// #510: everything that reads or rewrites cell content or moves cells
/// closes the editor first; formatting and dialog/bar openers that neither
/// read nor rewrite cell values, and the pick list, leave it open.
#[test]
fn acts_that_rewrite_cells_close_the_editor() {
    for act in [
        SheetAct::Paste,
        SheetAct::Cut,
        SheetAct::Copy,
        SheetAct::RemoveDuplicates,
        SheetAct::TextToColumns,
        SheetAct::Subtotal,
        SheetAct::Fill(gridcore::edit::FillDir::Down),
        SheetAct::Clear(gridcore::edit::ClearWhat::Contents),
        SheetAct::FormatAsTable,
        SheetAct::Filter,
        SheetAct::SortAsc,
        SheetAct::InsertRow,
        SheetAct::AutoSum,
    ] {
        assert!(act_commits_editor(act), "{act:?}");
    }
    for act in [
        SheetAct::Bold,
        SheetAct::Percent,
        SheetAct::FormatCells,
        SheetAct::Merge,
        SheetAct::ProtectSheet,
        SheetAct::Menu(sheet_menus::SheetMenu::Clear),
        // The pick list commits its own entry (sheet_complete::pick_value),
        // so it must not find the buffer committed ahead of it.
        SheetAct::PickList,
        SheetAct::PickItem(0),
        SheetAct::Todo,
    ] {
        assert!(!act_commits_editor(act), "{act:?}");
    }
}

/// #510 r1: picking after typing commits once — the pick replaces the buffer
/// and is the only commit, so one undo step restores the cell to empty.
#[test]
fn pick_value_after_typing_commits_once() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::text("apple"));
    put(&mut v, 1, 0, Cell::text("apple"));
    select(&mut v, 2, 0);
    type_fresh(&mut v, "ap");
    assert!(v.pick_value("apple"));
    assert_eq!(value(&v, 2, 0), CellValue::Text("apple".into()));
    assert!(v.editing.is_none());
    assert_eq!(v.undo.len(), 1, "exactly one commit: the pick's own");
}

/// #510: the commit lands on the editor's origin cell, not on whatever cell
/// the selection has moved to, and records exactly one undo step.
#[test]
fn close_editor_for_act_commits_the_buffer_to_its_origin() {
    let mut v = view();
    type_fresh(&mut v, "42");
    select(&mut v, 1, 1);
    assert_eq!(v.close_editor_for_act(), Some(true));
    assert_eq!(value(&v, 0, 0), CellValue::Number(42.0));
    assert_eq!(value(&v, 1, 1), CellValue::default(), "B2 stays empty");
    assert!(v.editing.is_none());
    assert_eq!(v.undo.len(), 1);
}

/// #510: a refused commit (a formula that does not parse) leaves the editor
/// open with its error and records nothing; the act must not run.
#[test]
fn close_editor_for_act_keeps_a_refused_editor_open() {
    let mut v = view();
    type_fresh(&mut v, "=SUM(A1");
    assert_eq!(v.close_editor_for_act(), Some(false));
    assert!(v.editing.is_some(), "the editor stays open");
    assert!(v.entry_error.is_some());
    assert!(v.undo.is_empty(), "a refused commit records no undo step");
}

/// #510: no editor open — nothing happens.
#[test]
fn close_editor_for_act_without_editor_is_a_noop() {
    let mut v = view();
    assert_eq!(v.close_editor_for_act(), None);
    assert!(v.undo.is_empty());
    assert!(v.entry_error.is_none());
}

/// #510: a seeded editor left untouched closes without writing or recording
/// an undo step, so the tab must not turn dirty for a byte-identical
/// workbook (the Enter path's guarantee, sheet-edit.uit's first case).
#[test]
fn close_editor_for_act_closes_an_untouched_editor_without_writing() {
    let mut v = view();
    put(&mut v, 0, 0, Cell::text("old"));
    v.begin_cell_edit(None);
    assert!(v.edit_untouched(), "seeded from the cell");
    assert_eq!(v.close_editor_for_act(), Some(false));
    assert!(v.editing.is_none());
    assert_eq!(value(&v, 0, 0), CellValue::Text("old".into()));
    assert!(v.undo.is_empty());
}
