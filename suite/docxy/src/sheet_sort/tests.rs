//! The suite's sorts (#691): A to Z, the Sort dialog, the Sort Warning and
//! the merged-cells refusal, driven through the tab's dialog stack.

use super::*;
use crate::sheet_filter::tests::{tab, view};
use ctlcore::json::Json;
use gridcore::sheet::Cell;

/// DAT-CASE-037's list A1:C6 (Region, Rep, Amount), the cursor on B2.
fn regions() -> DocTab {
    let mut t = tab();
    let v = view(&mut t);
    let sh = &mut v.pkg.workbook.sheets[0];
    for (c, h) in ["Region", "Rep", "Amount"].iter().enumerate() {
        sh.set_cell(0, c as u32, Cell::text(h));
    }
    for (i, (g, r, n)) in [
        ("West", "Eve", 5.0),
        ("East", "Ann", 1.0),
        ("North", "Dan", 4.0),
        ("East", "Cara", 3.0),
        ("South", "Bob", 2.0),
    ]
    .into_iter()
    .enumerate()
    {
        let row = i as u32 + 1;
        sh.set_cell(row, 0, Cell::text(g));
        sh.set_cell(row, 1, Cell::text(r));
        sh.set_cell(row, 2, Cell::number(n));
    }
    v.sel = (1, 1);
    v.anchor = (1, 1);
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    t
}

fn col(t: &mut DocTab, c: u32) -> Vec<String> {
    let v = view(t);
    (1..=5)
        .map(|r| match v.sheet().cell(r, c).map(|x| &x.value) {
            Some(CellValue::Text(s)) => s.clone(),
            Some(CellValue::Number(n)) => n.to_string(),
            _ => String::new(),
        })
        .collect()
}

fn press(t: &mut DocTab, button: &str) -> Result<(), String> {
    crate::dialog_host::dialog_click(t, button)
}

fn set(t: &mut DocTab, control: &str, value: Json) {
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap();
}

#[test]
fn a_to_z_sorts_the_list_around_the_cursor() {
    let mut t = regions();
    quick(&mut t, true).unwrap();
    assert_eq!(col(&mut t, 1), ["Ann", "Bob", "Cara", "Dan", "Eve"]);
    assert_eq!(col(&mut t, 0), ["East", "South", "East", "North", "West"]);
    assert_eq!(t.status.as_ref(), "Sorted 5 rows");
    assert!(t.dirty);
}

#[test]
fn a_partial_selection_asks_the_sort_warning() {
    for (answer, want) in [
        (
            "Expand the selection",
            ["East", "South", "East", "North", "West"],
        ),
        (
            "Continue with the current selection",
            ["West", "East", "North", "East", "South"],
        ),
    ] {
        let mut t = regions();
        view(&mut t).sel = (5, 1); // B2:B6
        quick(&mut t, true).unwrap();
        assert_eq!(t.dialogs.top().map(|d| d.id), Some("sort-warning"));
        assert_eq!(col(&mut t, 1)[0], "Eve", "nothing moved yet");
        set(&mut t, "what", Json::Str(answer.into()));
        press(&mut t, "Sort").unwrap();
        assert!(t.dialogs.top().is_none());
        assert_eq!(col(&mut t, 1), ["Ann", "Bob", "Cara", "Dan", "Eve"]);
        assert_eq!(col(&mut t, 0), want, "{answer}");
    }
}

#[test]
fn the_sort_dialog_takes_levels_a_custom_list_and_case() {
    let mut t = regions();
    open_dialog(&mut t).unwrap();
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.id, "sort");
    // "My data has headers" was guessed; the keys are the headers.
    assert!(
        d.controls
            .iter()
            .any(|c| c.name == "headers" && c.value == Value::Bool(true))
    );
    set(&mut t, "by1", Json::Str("Region".into()));
    set(&mut t, "order1", Json::Str("Custom List...".into()));
    set(
        &mut t,
        "with1",
        Json::Str("North, South, East, West".into()),
    );
    set(&mut t, "by2", Json::Str("Amount".into()));
    set(&mut t, "order2", Json::Str("Z to A".into()));
    press(&mut t, "OK").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(col(&mut t, 0), ["North", "South", "East", "East", "West"]);
    assert_eq!(col(&mut t, 1), ["Dan", "Bob", "Cara", "Ann", "Eve"]);
    // A custom list with nothing in it is refused, the dialog kept open.
    open_dialog(&mut t).unwrap();
    set(&mut t, "order1", Json::Str("Custom List...".into()));
    assert!(press(&mut t, "OK").is_err());
    assert_eq!(t.dialogs.top().map(|d| d.id), Some("sort"));
}

#[test]
fn left_to_right_lists_rows_and_merged_cells_refuse() {
    let mut t = tab();
    let v = view(&mut t);
    let sh = &mut v.pkg.workbook.sheets[0];
    for (c, (n, l)) in [(3.0, "c"), (1.0, "a"), (2.0, "b")].into_iter().enumerate() {
        sh.set_cell(0, c as u32, Cell::number(n));
        sh.set_cell(1, c as u32, Cell::text(l));
    }
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    open_dialog(&mut t).unwrap();
    set(
        &mut t,
        "orientation",
        Json::Str("Sort left to right".into()),
    );
    let by1 = t.dialogs.top().unwrap().controls[0].items.clone();
    assert_eq!(by1, ["Row 1", "Row 2"]);
    press(&mut t, "OK").unwrap();
    let v = view(&mut t);
    let row: Vec<String> = (0..3)
        .map(|c| match v.sheet().cell(1, c).map(|x| &x.value) {
            Some(CellValue::Text(s)) => s.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(row, ["a", "b", "c"]);
    // One two-cell merge in the list: Excel's refusal, nothing moved.
    let mut t = regions();
    view(&mut t).pkg.workbook.sheets[0]
        .merges
        .push((2, 0, 2, 1));
    let e = quick(&mut t, true).unwrap_err();
    assert_eq!(e, gridcore::edit::SORT_MERGED);
    assert_eq!(col(&mut t, 1)[0], "Eve");
    assert!(!t.dirty);
}
