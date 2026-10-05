//! The suite's filter commands and dialogs (#690), driven the way the
//! harness drives them: `dialog-set` and `dialog-click` on the tab's stack.

use super::*;
use crate::{Kind, SheetView, new_sheet_surface};
use ctlcore::json::Json;
use gridcore::sheet::{Cell, Xf};

pub(crate) fn tab() -> DocTab {
    DocTab {
        kind: Kind::Xlsx,
        title: "Sales.xlsx".into(),
        path: None,
        surface: new_sheet_surface(),
        dirty: false,
        status: "".into(),
        comments: vec![],
        tracked_comment_ids: Default::default(),
        comments_removed_all: false,
        used_comment_ids: Default::default(),
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
        dialogs: crate::dialog::DialogStack::default(),
        access: crate::open_mode::Access::default(),
        last_hot: Default::default(),
        converted_docx: None,
        pending_conversion: false,
        mail: Default::default(),
        import: Default::default(),
    }
}

pub(crate) fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet"),
    }
}

/// A1:C7: Rep, Units, When (dates), the cursor on A1.
fn list() -> DocTab {
    let mut t = tab();
    let v = view(&mut t);
    let rows = [
        ("Noor", 5.0, 45361.0),
        ("Cy", 12.0, 45364.0),
        ("Bo", 30.0, 45380.0),
        ("Noor", 41.0, 45000.0),
        ("Ann", 2.0, 45366.0),
        ("Cy", 18.0, 45367.0),
    ];
    let wb = &mut v.pkg.workbook;
    let mut xf = Xf::default();
    xf.set_code(Some("yyyy-mm-dd".into()));
    let date = wb.styles.intern(xf);
    let sh = &mut wb.sheets[0];
    for (c, h) in ["Rep", "Units", "When"].iter().enumerate() {
        sh.set_cell(0, c as u32, Cell::text(h));
    }
    for (i, (r, u, d)) in rows.iter().enumerate() {
        let row = i as u32 + 1;
        sh.set_cell(row, 0, Cell::text(r));
        sh.set_cell(row, 1, Cell::number(*u));
        sh.set_cell(
            row,
            2,
            Cell {
                style: date,
                ..Cell::number(*d)
            },
        );
    }
    v.sel = (0, 0);
    v.anchor = (0, 0);
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    v.engine.clock = Some(45364.0); // 2024-03-13
    t
}

fn shown(t: &mut DocTab) -> Vec<u32> {
    let v = view(t);
    (1..=6)
        .filter(|&r| !v.sheet().row_hidden(r))
        .map(|r| r + 1)
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

fn top_id(t: &DocTab) -> &'static str {
    t.dialogs.top().map_or("none", |d| d.id)
}

#[test]
fn filter_toggles_buttons_and_turning_it_off_shows_every_row() {
    let mut t = list();
    toggle(&mut t).unwrap();
    assert_eq!(t.status.as_ref(), "Filter on");
    assert!(t.dirty);
    let af = view(&mut t).sheet().auto_filter.clone().unwrap();
    assert_eq!(af.range, (0, 0, 6, 2));
    // The drop-down's checklist: only Cy.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    assert_eq!(top_id(&t), "filter-menu");
    set(&mut t, "values", Json::Arr(vec![Json::Str("Cy".into())]));
    press(&mut t, "OK").unwrap();
    assert_eq!(top_id(&t), "none");
    assert_eq!(shown(&mut t), vec![3, 7]);
    assert_eq!(t.status.as_ref(), "2 of 6 records found");
    // One undo step: back to every row.
    let v = view(&mut t);
    let snap = v.undo.pop().unwrap();
    v.restore(snap);
    assert_eq!(shown(&mut t), vec![2, 3, 4, 5, 6, 7]);
    // Filter again, then off: every row shows and the buttons go.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "values", Json::Arr(vec![Json::Str("Bo".into())]));
    press(&mut t, "OK").unwrap();
    toggle(&mut t).unwrap();
    assert!(view(&mut t).sheet().auto_filter.is_none());
    assert_eq!(shown(&mut t), vec![2, 3, 4, 5, 6, 7]);
}

#[test]
fn clear_with_nothing_filtered_leaves_the_tab_clean() {
    let mut t = list();
    toggle(&mut t).unwrap();
    t.dirty = false;
    let undo = view(&mut t).undo.len();
    run(&mut t, |wb, s, today| {
        gridcore::filter::clear(wb, s, None, today)
    })
    .unwrap();
    assert!(!t.dirty);
    assert_eq!(view(&mut t).undo.len(), undo);
    assert_eq!(t.status.as_ref(), "6 of 6 records found");
}

#[test]
fn the_typed_submenu_opens_custom_autofilter_and_top_10() {
    let mut t = list();
    toggle(&mut t).unwrap();
    // Units: Number Filters › Between... asks Custom AutoFilter.
    let d = menu_dialog(&t, 1).unwrap();
    let typed = d.controls.iter().find(|c| c.name == "typed").unwrap();
    assert_eq!(typed.label, "Number Filters:");
    t.dialogs.push(d);
    set(&mut t, "typed", Json::Str("Between...".into()));
    press(&mut t, "Apply Filter").unwrap();
    assert_eq!(top_id(&t), "custom-autofilter");
    set(&mut t, "val1", Json::Str("10".into()));
    set(&mut t, "val2", Json::Str("30".into()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![3, 4, 7]);
    // Top 10...: the top 2 items.
    t.dialogs.push(menu_dialog(&t, 1).unwrap());
    set(&mut t, "typed", Json::Str("Top 10...".into()));
    press(&mut t, "Apply Filter").unwrap();
    assert_eq!(top_id(&t), "top10-autofilter");
    set(&mut t, "n", Json::Str("2".into()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![4, 5]);
    // Text Filters › Begins With... on Rep.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "typed", Json::Str("Begins With...".into()));
    press(&mut t, "Apply Filter").unwrap();
    set(&mut t, "val1", Json::Str("n".into()));
    press(&mut t, "OK").unwrap();
    // AND with Top 2 on Units: Noor 41 only.
    assert_eq!(shown(&mut t), vec![5]);
    // Clear Filter From Units: Noor's rows.
    t.dialogs.push(menu_dialog(&t, 1).unwrap());
    press(&mut t, "Clear Filter").unwrap();
    assert_eq!(shown(&mut t), vec![2, 5]);
}

#[test]
fn date_filters_apply_a_period_and_search_narrows_the_list() {
    let mut t = list();
    toggle(&mut t).unwrap();
    let d = menu_dialog(&t, 2).unwrap();
    // The date tree: 2023, then 2024 › March › days.
    let values = d.controls.iter().find(|c| c.name == "values").unwrap();
    assert_eq!(values.items[0], "(Select All)");
    assert_eq!(&values.items[1..3], ["2023", "March"]);
    t.dialogs.push(d);
    set(&mut t, "typed", Json::Str("This Week".into()));
    press(&mut t, "Apply Filter").unwrap();
    // Sun 10 to Sat 16 March 2024.
    assert_eq!(shown(&mut t), vec![2, 3, 6, 7]);
    // Search narrows the Rep list; OK keeps the matches.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "search", Json::Str("o*r".into()));
    press(&mut t, "Search").unwrap();
    let values = t
        .dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == "values")
        .unwrap()
        .items
        .clone();
    assert_eq!(values, ["(Select All Search Results)", "Noor"]);
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![2]);
}

#[test]
fn filter_by_the_selected_cells_value_and_colour() {
    let mut t = list();
    let v = view(&mut t);
    let green = v.pkg.workbook.styles.intern(Xf {
        fill: Some((0, 176, 80)),
        ..Xf::default()
    });
    v.pkg.workbook.sheets[0]
        .cells
        .get_mut(&(2, 0))
        .unwrap()
        .style = green;
    v.sel = (1, 0);
    // The list has no filter yet: the command turns it on first.
    by_cell(&mut t, ByCell::Value).unwrap();
    assert_eq!(shown(&mut t), vec![2, 5]);
    view(&mut t).sel = (2, 0);
    by_cell(&mut t, ByCell::CellColor).unwrap();
    assert_eq!(shown(&mut t), vec![3]);
    assert!(by_cell(&mut t, ByCell::Icon).is_err());
    assert_eq!(t.status.as_ref(), "The cell shows no icon.");
}

#[test]
fn advanced_filter_in_place_and_to_another_sheet() {
    let mut t = list();
    let v = view(&mut t);
    let sh = &mut v.pkg.workbook.sheets[0];
    sh.set_cell(0, 5, Cell::text("Rep"));
    sh.set_cell(1, 5, Cell::text("Cy"));
    v.pkg.workbook.sheets.push(gridcore::sheet::Sheet {
        name: "Other".into(),
        ..Default::default()
    });
    t.dialogs.push(advanced_dialog(&t).unwrap());
    let list = t.dialogs.top().unwrap().controls[1].value.clone();
    assert_eq!(list, Value::Text("A1:C7".into()));
    set(&mut t, "criteria", Json::Str("F1:F2".into()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![3, 7]);
    // Copying to another sheet is refused, and the dialog stays.
    t.dialogs.push(advanced_dialog(&t).unwrap());
    set(
        &mut t,
        "action",
        Json::Str("Copy to another location".into()),
    );
    set(&mut t, "criteria", Json::Str("F1:F2".into()));
    set(&mut t, "copy", Json::Str("Other!A1".into()));
    let e = press(&mut t, "OK").unwrap_err();
    assert_eq!(e, gridcore::filter::ADVANCED_OTHER_SHEET);
    assert_eq!(top_id(&t), "advanced-filter");
}

#[test]
fn filter_and_sort_by_a_colour_from_the_drop_down() {
    let mut t = list();
    let v = view(&mut t);
    let green = v.pkg.workbook.styles.intern(Xf {
        fill: Some((0, 176, 80)),
        ..Xf::default()
    });
    for r in [2, 5] {
        v.pkg.workbook.sheets[0]
            .cells
            .get_mut(&(r, 0))
            .unwrap()
            .style = green;
    }
    toggle(&mut t).unwrap();
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    let d = t.dialogs.top().unwrap();
    let color = d.controls.iter().find(|c| c.name == "color").unwrap();
    assert_eq!(
        color.items,
        [
            "Cell Color No Fill",
            "Cell Color 00B050",
            "Font Color Automatic"
        ]
    );
    set(&mut t, "color", Json::Str("Cell Color 00B050".into()));
    press(&mut t, "Filter by Color").unwrap();
    assert_eq!(shown(&mut t), vec![3, 6]);
    // No Fill replaces it.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "color", Json::Str("Cell Color No Fill".into()));
    press(&mut t, "Filter by Color").unwrap();
    assert_eq!(shown(&mut t), vec![2, 4, 5, 7]);
    // Sort by Color: the green records on top, the rest in their order.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    press(&mut t, "Clear Filter").unwrap();
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "color", Json::Str("Cell Color 00B050".into()));
    press(&mut t, "Sort by Color").unwrap();
    let v = view(&mut t);
    let reps: Vec<String> = (1..=6)
        .map(|r| match v.sheet().cell(r, 0).map(|c| &c.value) {
            Some(gridcore::sheet::CellValue::Text(s)) => s.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(reps, ["Cy", "Ann", "Noor", "Bo", "Noor", "Cy"]);
}

/// The `values` check list's labels and checks.
fn checklist(t: &DocTab) -> Vec<(String, bool)> {
    let c = t
        .dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == "values")
        .unwrap();
    let Value::Checks(checks) = &c.value else {
        panic!("a check list")
    };
    c.items
        .iter()
        .cloned()
        .zip(checks.iter().copied())
        .collect()
}

#[test]
fn ok_reads_the_list_shown_not_the_search_typed_since() {
    // A search typed but not run: OK unchecks Bo from the full list.
    let mut t = list();
    toggle(&mut t).unwrap();
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "search", Json::Str("No".into()));
    t.dialogs
        .set(
            "values",
            &Json::obj(vec![
                ("item", Json::Str("Bo".into())),
                ("checked", Json::Bool(false)),
            ]),
        )
        .unwrap();
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![2, 3, 5, 6, 7]);
    // A search run, then the box cleared: OK keeps the search's results.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "search", Json::Str("oo".into()));
    press(&mut t, "Search").unwrap();
    assert_eq!(checklist(&t)[1].0, "Noor");
    set(&mut t, "search", Json::Str(String::new()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![2, 5]);
}

#[test]
fn ok_on_an_untouched_list_keeps_a_non_checklist_criterion() {
    let mut t = list();
    toggle(&mut t).unwrap();
    run(&mut t, |wb, s, today| {
        let top = ColumnFilter::Top10 {
            top: true,
            percent: false,
            val: 2.0,
            filter_val: None,
        };
        gridcore::filter::set_criterion(wb, s, 1, Some(top), today)
    })
    .unwrap();
    assert_eq!(shown(&mut t), vec![4, 5]);
    t.dialogs.push(menu_dialog(&t, 1).unwrap());
    // Nothing is checked: the column's filter isn't a checklist.
    assert!(checklist(&t).iter().all(|(_, on)| !on));
    press(&mut t, "OK").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(shown(&mut t), vec![4, 5], "the Top 10 stays");
}

#[test]
fn a_filter_command_that_fails_still_keeps_the_typed_value() {
    let mut t = list();
    let v = view(&mut t);
    v.sel = (3, 0);
    v.begin_cell_edit(None);
    v.editing = Some("Zed".into());
    // No filter on the sheet: Reapply refuses, but the typed value is in.
    assert!(run(&mut t, gridcore::filter::reapply).is_err());
    assert!(t.dirty, "the committed value makes the tab dirty");
    let v = view(&mut t);
    assert!(v.editing.is_none());
    assert_eq!(
        v.sheet().cell(3, 0).map(|c| c.value.clone()),
        Some(gridcore::sheet::CellValue::Text("Zed".into()))
    );
}

#[test]
fn a_typed_date_compares_as_a_date_only_on_a_date_column() {
    let mut t = list();
    // Rep is text: `1-2-3` is matched as typed.
    view(&mut t).pkg.workbook.sheets[0].set_cell(6, 0, Cell::text("x1-2-3y"));
    toggle(&mut t).unwrap();
    t.dialogs.push(custom_dialog(0, 0, "Rep", 10, false));
    set(&mut t, "val1", Json::Str("1-2-3".into()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![7]);
    // When is a date column: `2024-03-13` is that date.
    run(&mut t, |wb, s, today| {
        gridcore::filter::clear(wb, s, None, today)
    })
    .unwrap();
    t.dialogs.push(custom_dialog(0, 2, "When", 0, true));
    set(&mut t, "val1", Json::Str("2024-03-13".into()));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![3]);
}

#[test]
fn a_search_for_a_month_name_keeps_that_month() {
    // When shows yyyy-mm-dd, but the tree lists March: OK keeps the March
    // records, as the list showed them.
    let mut t = list();
    let v = view(&mut t);
    let april = Cell {
        style: v.sheet().cell(6, 2).unwrap().style,
        ..Cell::number(45400.0) // 2024-04-18
    };
    v.pkg.workbook.sheets[0].set_cell(6, 2, april);
    toggle(&mut t).unwrap();
    t.dialogs.push(menu_dialog(&t, 2).unwrap());
    set(&mut t, "search", Json::Str("March".into()));
    press(&mut t, "Search").unwrap();
    assert!(checklist(&t).iter().any(|(l, _)| l == "March"));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![2, 3, 4, 5, 6]);
}

#[test]
fn add_to_filter_takes_the_searched_checks_and_keeps_the_old_values() {
    let mut t = list();
    toggle(&mut t).unwrap();
    run(&mut t, |wb, s, today| {
        let ann = ColumnFilter::values(vec!["Ann".into()]);
        gridcore::filter::set_criterion(wb, s, 0, Some(ann), today)
    })
    .unwrap();
    // Search "o": Bo and Noor; Bo unchecked, added to the filter.
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    set(&mut t, "search", Json::Str("o".into()));
    press(&mut t, "Search").unwrap();
    t.dialogs
        .set(
            "values",
            &Json::obj(vec![
                ("item", Json::Str("Bo".into())),
                ("checked", Json::Bool(false)),
            ]),
        )
        .unwrap();
    set(&mut t, "add", Json::Bool(true));
    press(&mut t, "OK").unwrap();
    assert_eq!(shown(&mut t), vec![2, 5, 6], "Noor, Noor and Ann");
}

#[test]
fn ok_with_nothing_checked_is_refused_and_the_drop_down_stays() {
    let mut t = list();
    toggle(&mut t).unwrap();
    t.dialogs.push(menu_dialog(&t, 0).unwrap());
    t.dialogs
        .set(
            "values",
            &Json::obj(vec![
                ("item", Json::Str("(Select All)".into())),
                ("checked", Json::Bool(false)),
            ]),
        )
        .unwrap();
    assert_eq!(
        press(&mut t, "OK").unwrap_err(),
        "Select at least one item."
    );
    assert_eq!(top_id(&t), "filter-menu");
    // A search nothing matches: no results to check, the same refusal.
    set(&mut t, "search", Json::Str("zz".into()));
    press(&mut t, "Search").unwrap();
    assert_eq!(checklist(&t).len(), 1, "only (Select All Search Results)");
    assert!(press(&mut t, "OK").is_err());
    assert_eq!(top_id(&t), "filter-menu");
    assert_eq!(shown(&mut t), vec![2, 3, 4, 5, 6, 7], "nothing hidden");
}

#[test]
fn a_filter_command_under_an_entry_that_cannot_commit_says_why() {
    let mut t = list();
    toggle(&mut t).unwrap();
    let v = view(&mut t);
    v.sel = (3, 0);
    v.begin_cell_edit(Some("y".repeat(32_768)));
    let e = run(&mut t, gridcore::filter::reapply).unwrap_err();
    assert!(e.contains("32767"), "the entry's own reason: {e}");
    assert!(view(&mut t).editing.is_some(), "the editor stays open");
}
