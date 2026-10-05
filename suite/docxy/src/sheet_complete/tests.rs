//! Pick From Drop-down List and Formula AutoComplete (#665, #686) on
//! `SheetView`, without a window; `uiharness/cases/sheet-autocomplete.uit`
//! and `sheet-formula-complete.uit` drive the same steps through keys.

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

/// Type into a fresh editor on the selected cell, key by key.
fn type_keys(v: &mut SheetView, text: &str) {
    for ch in text.chars() {
        v.type_char(&ch.to_string());
    }
}

fn text(v: &SheetView, r: u32, c: u32) -> String {
    match v.sheet().cell(r, c).map(|c| c.value.clone()) {
        Some(CellValue::Text(t)) => t,
        other => panic!("{other:?}"),
    }
}

/// ENT-CASE-029's column: A1:A5 `Widgets`, `Gadgets`, `Gizmos`, `100`,
/// `Grommets`, A7 `Sprockets`.
fn widgets() -> SheetView {
    let mut v = view();
    for (r, t) in ["Widgets", "Gadgets", "Gizmos", "100", "Grommets"]
        .iter()
        .enumerate()
    {
        put(&mut v, r as u32, 0, t);
    }
    put(&mut v, 6, 0, "Sprockets");
    v
}

fn mods(alt: bool, shift: bool, control: bool) -> Modifiers {
    Modifiers {
        alt,
        shift,
        control,
        ..Modifiers::default()
    }
}

#[test]
fn alt_down_is_routed_past_the_keytips() {
    assert!(alt_down_key("down", mods(true, false, false)));
    assert!(!alt_down_key("down", mods(false, false, false)));
    assert!(
        !alt_down_key("down", mods(true, true, false)),
        "Alt+Shift+Down"
    );
    assert!(
        !alt_down_key("down", mods(true, false, true)),
        "Ctrl+Alt+Down"
    );
    assert!(!alt_down_key("up", mods(true, false, false)));
}

#[test]
fn alt_down_opens_in_excels_order() {
    let mut v = widgets();
    select(&mut v, 7, 0);
    assert_eq!(alt_down_target(&v, false), AltDown::PickList);
    assert_eq!(alt_down_target(&v, true), AltDown::Validation);
    // A filter header's button comes first.
    v.pkg.workbook.sheets[0].auto_filter = Some(gridcore::sheet::SheetAutoFilter {
        range: (7, 0, 9, 1),
        columns: Vec::new(),
        criteria: Vec::new(),
    });
    assert_eq!(alt_down_target(&v, true), AltDown::FilterButton(0));
    select(&mut v, 8, 0);
    assert_eq!(
        alt_down_target(&v, false),
        AltDown::PickList,
        "below the header"
    );
    // While typing: a formula's names, else the pick list.
    v.begin_cell_edit(Some("=SU".into()));
    assert_eq!(alt_down_target(&v, true), AltDown::Completions);
    v.editing = Some("Wid".into());
    assert_eq!(alt_down_target(&v, true), AltDown::PickList);
}

#[test]
fn ent_case_029_autocomplete_through_typing() {
    let mut v = widgets();
    select(&mut v, 7, 0);
    type_keys(&mut v, "wid");
    assert_eq!(v.editing.as_deref(), Some("wid"), "Widgets is beyond A6");
    v.end_cell_edit();
    type_keys(&mut v, "spr");
    // The typed letters stay as typed while the proposal shows (#672 pins
    // this, as `APple`); the commit takes the column's case.
    assert_eq!(v.editing.as_deref(), Some("sprockets"));
    assert_eq!(v.edit_proposal, Some((3, "Sprockets".to_string())));
    assert!(v.commit_edit());
    assert_eq!(text(&v, 7, 0), "Sprockets");
    select(&mut v, 5, 0);
    type_keys(&mut v, "gadg");
    assert!(v.commit_edit());
    assert_eq!(text(&v, 5, 0), "Gadgets", "between two blocks");
    select(&mut v, 8, 0);
    type_keys(&mut v, "g");
    assert_eq!(v.editing.as_deref(), Some("g"), "ambiguous");
    v.end_cell_edit();
    type_keys(&mut v, "giz");
    assert!(v.commit_edit());
    assert_eq!(text(&v, 8, 0), "Gizmos");
    select(&mut v, 9, 0);
    type_keys(&mut v, "1");
    assert_eq!(
        v.editing.as_deref(),
        Some("1"),
        "numbers are never completed"
    );
    v.end_cell_edit();
    type_keys(&mut v, "widg");
    assert!(v.commit_edit());
    assert_eq!(text(&v, 9, 0), "Widgets");
}

#[test]
fn ent_case_030_the_pick_list_and_its_choice() {
    let mut v = widgets();
    put(&mut v, 5, 0, "Widgets");
    select(&mut v, 7, 0);
    let values = v.pick_values();
    assert_eq!(
        values,
        ["Gadgets", "Gizmos", "Grommets", "Sprockets", "Widgets"]
    );
    let items = menu::pick_menu(&values);
    assert_eq!(items.len(), 5);
    let steps = v.undo.len();
    assert!(v.pick_value(&values[0]));
    assert_eq!(text(&v, 7, 0), "Gadgets");
    assert_eq!(v.undo.len(), steps + 1, "one undo step");
    // While typing: the choice replaces the text and commits.
    select(&mut v, 8, 0);
    type_keys(&mut v, "x");
    let values = v.pick_values();
    assert!(v.pick_value(&values[1]));
    assert!(v.editing.is_none());
    assert_eq!(text(&v, 8, 0), "Gizmos");
    // No block around the cell: nothing to pick.
    select(&mut v, 0, 5);
    assert!(v.pick_values().is_empty());
}

/// TBL-CASE-021's table `Sales` (Item, Qty, Price, Region) over A1:D4.
fn sales() -> SheetView {
    let mut v = view();
    for (c, h) in ["Item", "Qty", "Price", "Region"].iter().enumerate() {
        put(&mut v, 0, c as u32, h);
    }
    v.pkg.workbook.tables.push(gridcore::sheet::Table {
        name: "Sales".into(),
        sheet: 0,
        range: (0, 0, 3, 3),
        header_rows: 1,
        totals_rows: 0,
        columns: ["Item", "Qty", "Price", "Region"]
            .map(String::from)
            .to_vec(),
        column_ids: Vec::new(),
        part: String::new(),
    });
    v
}

fn listed(v: &SheetView) -> Vec<String> {
    v.complete_view()
        .map(|c| c.list.items.into_iter().map(|i| i.label).collect())
        .unwrap_or_default()
}

#[test]
fn tbl_case_021_the_formula_list_follows_the_typing() {
    let mut v = sales();
    select(&mut v, 0, 7);
    type_keys(&mut v, "=SUM(Sal");
    assert_eq!(listed(&v), ["Sales"]);
    type_keys(&mut v, "es[");
    assert_eq!(
        listed(&v),
        [
            "Item",
            "Qty",
            "Price",
            "Region",
            "#All",
            "#Data",
            "#Headers",
            "#Totals",
            "@ - This Row"
        ]
    );
    assert!(v.complete_open());
    v.complete_step(true);
    assert_eq!(v.complete_view().unwrap().sel, 1);
    assert!(v.complete_insert());
    assert_eq!(v.editing.as_deref(), Some("=SUM(Sales[Qty"));
    assert_eq!(v.edit_caret, 14);
    assert!(listed(&v).is_empty(), "closed after the insert");
    type_keys(&mut v, "])");
    assert!(v.commit_edit());
    assert_eq!(
        v.sheet().cell(0, 7).unwrap().formula.as_deref(),
        Some("SUM(Sales[Qty])")
    );
}

#[test]
fn the_formula_list_keys_highlight_and_option() {
    let mut v = sales();
    select(&mut v, 1, 7);
    type_keys(&mut v, "=su");
    assert_eq!(listed(&v)[..3], ["SUBSTITUTE", "SUBTOTAL", "SUM"]);
    v.complete_now(false);
    v.complete_step(true);
    v.complete_step(true);
    v.complete_step(false);
    assert_eq!(v.complete_view().unwrap().sel, 1);
    // The highlight stays on its item as the prefix narrows.
    type_keys(&mut v, "b");
    let c = v.complete_view().unwrap();
    assert_eq!(c.list.items[c.sel].label, "SUBTOTAL");
    assert!(v.complete_insert());
    assert_eq!(v.editing.as_deref(), Some("=SUBTOTAL("));
    // Esc closes the list, not the editor; typing opens it again.
    type_keys(&mut v, "9,S");
    assert!(v.complete_open());
    v.complete_close();
    assert!(!v.complete_open());
    assert!(v.editing.is_some());
    type_keys(&mut v, "u");
    assert!(v.complete_open());
    // Not in text; off with the option; Alt+Down on demand anyway.
    v.end_cell_edit();
    type_keys(&mut v, "su");
    assert!(listed(&v).is_empty());
    v.end_cell_edit();
    v.edit_opts.formula_autocomplete = false;
    type_keys(&mut v, "=su");
    assert!(listed(&v).is_empty());
    assert!(v.complete_now(true).is_some());
    assert_eq!(listed(&v)[2], "SUM");
}

#[test]
fn enter_commits_a_formula_as_typed_while_the_list_shows() {
    let mut v = sales();
    select(&mut v, 2, 7);
    type_keys(&mut v, "=Sal");
    assert!(v.complete_open());
    // `=Sal` is a name the workbook does not define: committed as typed,
    // nothing inserted.
    assert!(v.commit_edit());
    assert_eq!(
        v.sheet().cell(2, 7).unwrap().formula.as_deref(),
        Some("Sal")
    );
}
