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
