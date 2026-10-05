//! Flash Fill on `SheetView` (#666), without a window;
//! `uiharness/cases/sheet-flashfill.uit` drives it through keys.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::CellValue;

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn put(v: &mut SheetView, r: u32, c: u32, text: &str) {
    let s = v.active;
    let cell = gridcore::entry::entry_cell(&mut v.pkg.workbook, s, r, c, text, None).unwrap();
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn select(v: &mut SheetView, r: u32, c: u32) {
    v.sel = (r, c);
    v.anchor = (r, c);
}

fn value(v: &SheetView, r: u32, c: u32) -> CellValue {
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn text(v: &SheetView, r: u32, c: u32) -> String {
    match value(v, r, c) {
        CellValue::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

/// Type `t` into (r, c) and commit with Enter, as `sheet_commit_move` does
/// (the preview included).
fn type_enter(v: &mut SheetView, r: u32, c: u32, t: &str) {
    select(v, r, c);
    for ch in t.chars() {
        v.type_char(&ch.to_string());
    }
    let origin = v.edit_origin.unwrap();
    assert_eq!(v.commit_and_move(1, 0), Some(true));
    v.flash_preview_after(origin);
}

const NAMES: [&str; 6] = [
    "Ada Lovelace",
    "Alan Turing",
    "Grace Hopper",
    "Edsger Dijkstra",
    "Barbara Liskov",
    "Donald Knuth",
];

/// ENT-CASE-038's sheet: A1:A6 names, C1:C3 `x-007`, `y-042`, `z-100`.
fn names() -> SheetView {
    let mut v = view();
    for (r, t) in NAMES.iter().enumerate() {
        put(&mut v, r as u32, 0, t);
    }
    for (r, t) in ["x-007", "y-042", "z-100"].iter().enumerate() {
        put(&mut v, r as u32, 2, t);
    }
    v
}

#[test]
fn ent_case_038_fills_constants_keeps_values_and_text() {
    let mut v = names();
    put(&mut v, 4, 1, "Babs");
    type_enter(&mut v, 0, 1, "Ada");
    assert_eq!(v.sel, (1, 1));
    let steps = v.undo.len();
    let f = v.flash_fill_at(1, 1).unwrap();
    assert_eq!(f.fills.len(), 4);
    let col: Vec<String> = (0..6).map(|r| text(&v, r, 1)).collect();
    assert_eq!(col, ["Ada", "Alan", "Grace", "Edsger", "Babs", "Donald"]);
    assert!(v.sheet().cell(1, 1).unwrap().formula.is_none());
    assert_eq!(v.undo.len(), steps + 1, "one undo step");
    // Constants: a changed source leaves them.
    type_enter(&mut v, 0, 0, "Augusta Lovelace");
    assert_eq!(text(&v, 1, 1), "Alan");
    // `'007` gives text.
    type_enter(&mut v, 0, 3, "'007");
    v.flash_fill_at(0, 3).unwrap();
    assert_eq!(value(&v, 1, 3), CellValue::Text("042".into()));
    assert_eq!(value(&v, 2, 3), CellValue::Text("100".into()));
}

#[test]
fn ent_case_039_preview_enter_options_and_no_pattern() {
    let mut v = names();
    type_enter(&mut v, 0, 1, "Ada");
    assert!(v.live_preview().is_none(), "one example");
    type_enter(&mut v, 1, 1, "Alan");
    let p = v
        .live_preview()
        .expect("a preview after the second example");
    let shown: Vec<&str> = p.fill.fills.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(shown, ["Grace", "Edsger", "Barbara", "Donald"]);
    assert_eq!(
        value(&v, 2, 1),
        CellValue::Empty,
        "a preview writes nothing"
    );
    // Enter accepts exactly those values, as one undo step.
    let steps = v.undo.len();
    assert!(v.accept_preview());
    assert_eq!(v.undo.len(), steps + 1);
    let col: Vec<String> = (2..6).map(|r| text(&v, r, 1)).collect();
    assert_eq!(col, ["Grace", "Edsger", "Barbara", "Donald"]);
    assert!(v.live_preview().is_none());
    // The Options menu counts them.
    let f = v.live_flash().expect("the options button").clone();
    let labels: Vec<String> = options_menu(&f)
        .into_iter()
        .filter_map(|i| match i {
            menu::MenuItem::Item(e) => Some(e.label),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "Undo Flash Fill",
            "Accept suggestions",
            "Select all 0 blank cells",
            "Select all 4 changed cells"
        ]
    );
    assert_eq!(f.button_cell(), Some((5, 1)));
    assert!(v.flash_select(false));
    assert_eq!((v.sel, v.anchor), ((2, 1), (5, 1)), "B3:B6");
    assert!(!v.flash_select(true), "no blank cells");
    // Undo Flash Fill is the fill's own undo step.
    assert!(v.undo_step());
    assert_eq!(value(&v, 2, 1), CellValue::Empty);
    assert!(v.live_flash().is_none(), "the button goes with the fill");
    // E1 `zzz`: no pattern, nothing filled.
    for (r, t) in ["'007", "'042", "'100"].iter().enumerate() {
        put(&mut v, r as u32, 3, t);
    }
    type_enter(&mut v, 0, 4, "zzz");
    let msg = v.flash_fill_at(0, 4).unwrap_err();
    assert!(msg.starts_with("Flash Fill didn't see a pattern"), "{msg}");
    assert_eq!(value(&v, 1, 4), CellValue::Empty);
}

#[test]
fn the_preview_goes_with_any_move_and_obeys_the_option() {
    let mut v = names();
    type_enter(&mut v, 0, 1, "Ada");
    type_enter(&mut v, 1, 1, "Alan");
    assert!(v.live_preview().is_some());
    select(&mut v, 7, 7);
    assert!(v.live_preview().is_none(), "a selection move");
    select(&mut v, 2, 1);
    assert!(v.live_preview().is_some(), "back where it was made");
    put(&mut v, 9, 9, "x");
    v.push_undo();
    assert!(v.live_preview().is_none(), "an edit");
    assert!(!v.accept_preview());
    // Automatically Flash Fill off: no preview.
    let mut v = names();
    v.edit_opts.flash_fill_auto = false;
    type_enter(&mut v, 0, 1, "Ada");
    type_enter(&mut v, 1, 1, "Alan");
    assert!(v.live_preview().is_none());
}
