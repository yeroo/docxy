//! Page Layout's commands and Page Setup dialog (#1019) on a `DocTab`,
//! without a window. `uiharness/cases/sheet-ribbon.uit` drives the same
//! commands through the ribbon.

use super::*;
use crate::{Kind, SheetAct, new_sheet_surface};
use ctlcore::json::Json;
use gridcore::sheet::Sheet;

fn tab() -> DocTab {
    DocTab {
        kind: Kind::Xlsx,
        title: "book.xlsx".into(),
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

fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet"),
    }
}

fn sheet(t: &mut DocTab) -> &Sheet {
    view(t).sheet()
}

fn setup(t: &mut DocTab) -> PageSetup {
    sheet(t).page_setup.clone()
}

fn select(t: &mut DocTab, r0: u32, c0: u32, r1: u32, c1: u32) {
    let v = view(t);
    v.anchor = (r0, c0);
    v.sel = (r1, c1);
}

fn undo_len(t: &mut DocTab) -> usize {
    view(t).undo.len()
}

fn print_area(t: &mut DocTab) -> Vec<area::Rect> {
    let v = view(t);
    area::print_area(&v.pkg.workbook, v.active)
}

fn on(t: &mut DocTab, act: PageAct) -> bool {
    let v = view(t);
    is_on(act, &v.pkg.workbook, v.active)
}

/// `act` changed nothing: no undo step, a clean tab, the layout as it was.
fn assert_no_change(t: &mut DocTab, act: PageAct) {
    let before = {
        let v = view(t);
        layout_state(&v.pkg.workbook, v.active)
    };
    let steps = undo_len(t);
    run(t, act);
    assert_eq!(undo_len(t), steps, "{act:?} took an undo step");
    assert!(!t.dirty, "{act:?} dirtied the tab");
    let v = view(t);
    assert!(layout_state(&v.pkg.workbook, v.active) == before, "{act:?}");
}

#[test]
fn set_print_area_takes_the_selection_as_one_undo_step() {
    let mut t = tab();
    select(&mut t, 0, 0, 4, 2);
    run(&mut t, PageAct::Area(AreaOp::Set));
    assert_eq!(print_area(&mut t), [(0, 0, 4, 2)]);
    assert_eq!(undo_len(&mut t), 1);
    assert!(t.dirty);
    assert!(view(&mut t).undo_step());
    assert!(print_area(&mut t).is_empty());
}

#[test]
fn set_and_add_to_print_area_take_every_area() {
    let mut t = tab();
    {
        let v = view(&mut t);
        v.anchor = (0, 0);
        v.sel = (1, 1);
        v.areas = vec![(5, 0, 6, 0)];
        v.areas_at = (v.active, v.sel, v.anchor);
    }
    assert!(view(&mut t).multi_area());
    run(&mut t, PageAct::Area(AreaOp::Set));
    assert_eq!(print_area(&mut t), [(5, 0, 6, 0), (0, 0, 1, 1)]);
    // Add to Print Area appends a one-area selection.
    let mut t = tab();
    select(&mut t, 0, 0, 1, 1);
    run(&mut t, PageAct::Area(AreaOp::Set));
    select(&mut t, 9, 3, 9, 3);
    run(&mut t, PageAct::Area(AreaOp::Add));
    assert_eq!(print_area(&mut t), [(0, 0, 1, 1), (9, 3, 9, 3)]);
    assert_eq!(undo_len(&mut t), 2);
    run(&mut t, PageAct::Area(AreaOp::Clear));
    assert!(print_area(&mut t).is_empty());
    assert_eq!(undo_len(&mut t), 3);
}

#[test]
fn commands_that_change_nothing_take_no_step_and_leave_the_tab_clean() {
    let mut t = tab();
    select(&mut t, 3, 2, 3, 2);
    for act in [
        PageAct::Area(AreaOp::Clear),
        PageAct::Break(BreakOp::Remove),
        PageAct::Break(BreakOp::Reset),
        // A new sheet's `default` orientation prints portrait.
        PageAct::Landscape(false),
        PageAct::Paper(1),
        PageAct::Margins(MarginPreset::Normal),
        PageAct::FitWidth(0),
        PageAct::FitHeight(0),
        PageAct::Scale(100),
    ] {
        assert_no_change(&mut t, act);
    }
    // Portrait did not write `portrait` over `default`.
    assert_eq!(setup(&mut t).orientation, Orientation::Default);
    // Insert Page Break at A1 has nowhere to break.
    select(&mut t, 0, 0, 0, 0);
    assert_no_change(&mut t, PageAct::Break(BreakOp::Insert));
}

#[test]
fn breaks_insert_remove_and_reset_at_the_active_cell() {
    let mut t = tab();
    select(&mut t, 3, 2, 3, 2);
    run(&mut t, PageAct::Break(BreakOp::Insert));
    assert_eq!(area::manual_breaks(sheet(&mut t)), (vec![3], vec![2]));
    run(&mut t, PageAct::Break(BreakOp::Remove));
    assert_eq!(area::manual_breaks(sheet(&mut t)), (vec![], vec![]));
    select(&mut t, 5, 0, 5, 0);
    run(&mut t, PageAct::Break(BreakOp::Insert));
    select(&mut t, 0, 4, 0, 4);
    run(&mut t, PageAct::Break(BreakOp::Insert));
    assert_eq!(area::manual_breaks(sheet(&mut t)), (vec![5], vec![4]));
    run(&mut t, PageAct::Break(BreakOp::Reset));
    assert_eq!(area::manual_breaks(sheet(&mut t)), (vec![], vec![]));
    assert_eq!(undo_len(&mut t), 5);
}

#[test]
fn orientation_size_and_margins_write_the_choice_and_tick_it() {
    let mut t = tab();
    assert!(on(&mut t, PageAct::Landscape(false)));
    run(&mut t, PageAct::Landscape(true));
    assert_eq!(setup(&mut t).orientation, Orientation::Landscape);
    assert!(on(&mut t, PageAct::Landscape(true)) && !on(&mut t, PageAct::Landscape(false)));
    run(&mut t, PageAct::Landscape(false));
    assert_eq!(setup(&mut t).orientation, Orientation::Portrait);
    run(&mut t, PageAct::Paper(9));
    assert_eq!(setup(&mut t).paper_size, 9);
    assert!(on(&mut t, PageAct::Paper(9)) && !on(&mut t, PageAct::Paper(1)));
    assert!(on(&mut t, PageAct::Margins(MarginPreset::Normal)));
    run(&mut t, PageAct::Margins(MarginPreset::Wide));
    let m = setup(&mut t).margins;
    assert_eq!(
        (m.left, m.right, m.top, m.bottom, m.header, m.footer),
        (1.0, 1.0, 1.0, 1.0, 0.5, 0.5)
    );
    run(&mut t, PageAct::Margins(MarginPreset::Narrow));
    let m = setup(&mut t).margins;
    assert_eq!(
        (m.left, m.right, m.top, m.bottom, m.header, m.footer),
        (0.25, 0.25, 0.75, 0.75, 0.3, 0.3)
    );
    assert!(on(&mut t, PageAct::Margins(MarginPreset::Narrow)));
    assert!(!on(&mut t, PageAct::Margins(MarginPreset::Normal)));
    assert_eq!(undo_len(&mut t), 5);
    // Each was its own step.
    assert!(view(&mut t).undo_step());
    assert!(on(&mut t, PageAct::Margins(MarginPreset::Wide)));
}

#[test]
fn scale_to_fit_follows_excels_rules() {
    let mut t = tab();
    assert_eq!(shown_fit(&setup(&mut t)), (0, 0));
    assert!(on(&mut t, PageAct::Scale(100)));
    // A width fits the sheet to pages; the height stays Automatic.
    run(&mut t, PageAct::FitWidth(1));
    let p = setup(&mut t);
    assert!(p.fit_to_page);
    assert_eq!((p.fit_width, p.fit_height), (1, 0));
    assert!(on(&mut t, PageAct::FitWidth(1)) && on(&mut t, PageAct::FitHeight(0)));
    assert!(!on(&mut t, PageAct::Scale(100)));
    run(&mut t, PageAct::FitHeight(3));
    assert_eq!(shown_fit(&setup(&mut t)), (1, 3));
    // Both Automatic again prints at the scale.
    run(&mut t, PageAct::FitWidth(0));
    assert_eq!(shown_fit(&setup(&mut t)), (0, 3));
    run(&mut t, PageAct::FitHeight(0));
    assert!(!setup(&mut t).fit_to_page);
    // A scale turns fitting off and sets the percentage.
    run(&mut t, PageAct::FitWidth(2));
    run(&mut t, PageAct::Scale(75));
    let p = setup(&mut t);
    assert!(!p.fit_to_page);
    assert_eq!(p.scale, 75);
    assert_eq!(shown_fit(&p), (0, 0));
    assert!(on(&mut t, PageAct::Scale(75)));
    assert_eq!(undo_len(&mut t), 6);
}

#[test]
fn print_gridlines_and_headings_toggle_and_read_checked() {
    let mut t = tab();
    for (act, get) in [
        (
            PageAct::PrintGridlines,
            (|p: &PageSetup| p.grid_lines) as fn(&PageSetup) -> bool,
        ),
        (PageAct::PrintHeadings, |p: &PageSetup| p.headings),
    ] {
        assert!(!on(&mut t, act));
        run(&mut t, act);
        assert!(get(&setup(&mut t)) && on(&mut t, act));
        run(&mut t, act);
        assert!(!get(&setup(&mut t)) && !on(&mut t, act));
        run(&mut t, act);
        assert!(view(&mut t).undo_step());
        assert!(!on(&mut t, act));
    }
}

#[test]
fn a_protected_sheet_still_takes_page_setup_print_area_and_breaks() {
    let mut t = tab();
    view(&mut t).pkg.workbook.sheets[0].protection = Some(r#"sheet="1""#.into());
    assert!(sheet(&mut t).is_protected());
    select(&mut t, 2, 1, 2, 1);
    run(&mut t, PageAct::Area(AreaOp::Set));
    run(&mut t, PageAct::Break(BreakOp::Insert));
    run(&mut t, PageAct::Landscape(true));
    run(&mut t, PageAct::PrintGridlines);
    assert_eq!(print_area(&mut t), [(2, 1, 2, 1)]);
    assert_eq!(area::manual_breaks(sheet(&mut t)), (vec![2], vec![1]));
    assert!(setup(&mut t).orientation.is_landscape() && setup(&mut t).grid_lines);
    assert_eq!(undo_len(&mut t), 4);
}

#[test]
fn page_acts_sort_into_the_command_tables() {
    use crate::{act_commits_editor, act_targets_cells, multi_area_ok, protected_view_allows_act};
    let all = [
        PageAct::Area(AreaOp::Set),
        PageAct::Area(AreaOp::Clear),
        PageAct::Area(AreaOp::Add),
        PageAct::Break(BreakOp::Insert),
        PageAct::Break(BreakOp::Remove),
        PageAct::Break(BreakOp::Reset),
        PageAct::Landscape(true),
        PageAct::Paper(9),
        PageAct::Margins(MarginPreset::Wide),
        PageAct::FitWidth(1),
        PageAct::FitHeight(1),
        PageAct::Scale(50),
        PageAct::PrintGridlines,
        PageAct::PrintHeadings,
        PageAct::Dialog(SetupTab::Page),
        PageAct::Dialog(SetupTab::Sheet),
    ];
    for p in all {
        let act = SheetAct::Page(p);
        // Every one writes the file (the dialog's OK does), so Protected
        // View refuses them; none acts on one area of several.
        assert!(!protected_view_allows_act(act), "{p:?}");
        assert!(multi_area_ok(act), "{p:?}");
        let reads_selection = matches!(p, PageAct::Area(_) | PageAct::Break(_));
        assert_eq!(act_targets_cells(act), reads_selection, "{p:?}");
        assert_eq!(act_commits_editor(act), reads_selection, "{p:?}");
    }
    // AutoSum writes into one cell of one area.
    assert!(!multi_area_ok(SheetAct::AutoSumFn(crate::SumFn::Max)));
    assert!(act_commits_editor(SheetAct::AutoSumFn(crate::SumFn::Max)));
}

// ---- the Page Setup dialog ----------------------------------------------------

/// Set `control`, on whichever tab holds it (as `dialog-tab` then
/// `dialog-set` would).
fn set(t: &mut DocTab, control: &str, value: Json) {
    let d = t.dialogs.top_dialog_mut().unwrap();
    if let Some(page) = d
        .controls
        .iter()
        .find(|c| c.name == control)
        .and_then(|c| c.page)
    {
        d.tab = page;
    }
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap();
}

fn open(t: &mut DocTab) {
    run(t, PageAct::Dialog(SetupTab::Page));
    assert_eq!(t.dialogs.top().map(|d| d.id.as_str()), Some("page-setup"));
}

fn ok(t: &mut DocTab) {
    crate::dialog_host::dialog_click(t, "OK").unwrap();
    assert!(t.dialogs.top().is_none());
}

fn defined(t: &mut DocTab, name: &str) -> Option<String> {
    view(t)
        .pkg
        .workbook
        .defined_names
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.formula.clone())
}

#[test]
fn the_dialog_has_three_tabs_and_its_buttons_on_none() {
    let mut t = tab();
    for (at, i) in [
        (SetupTab::Page, 0),
        (SetupTab::Margins, 1),
        (SetupTab::Sheet, 2),
    ] {
        run(&mut t, PageAct::Dialog(at));
        let d = t.dialogs.top().unwrap();
        assert_eq!(d.tabs, ["Page", "Margins", "Sheet"]);
        assert_eq!(d.tab, i, "{at:?}");
        // Every control sits on a tab, so none makes one page too tall to
        // reach OK; the buttons are not controls and show on every tab.
        assert!(d.controls.iter().all(|c| c.page.is_some_and(|p| p < 3)));
        let labels: Vec<&str> = d.buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["OK", "Cancel"]);
        let page_of = |name: &str| d.controls.iter().find(|c| c.name == name).unwrap().page;
        assert_eq!(page_of("scale"), Some(0));
        assert_eq!(page_of("left"), Some(1));
        assert_eq!(page_of("title-rows"), Some(2));
        crate::dialog_host::dialog_click(&mut t, "Cancel").unwrap();
    }
}

#[test]
fn an_untouched_ok_keeps_full_precision_margins() {
    let mut t = tab();
    // As a file written in centimetres loads: 1.8 cm.
    let exact = 0.708_661_417_322_834_7;
    view(&mut t).pkg.workbook.sheets[0].page_setup.margins.left = exact;
    open(&mut t);
    assert_eq!(text(t.dialogs.top().unwrap(), "left"), "0.7087");
    ok(&mut t);
    assert_eq!(setup(&mut t).margins.left, exact);
    assert_eq!(undo_len(&mut t), 0);
    assert!(!t.dirty);
    // Changing another field writes that field and leaves the margin be.
    open(&mut t);
    set(&mut t, "gridlines", Json::Bool(true));
    ok(&mut t);
    assert_eq!(setup(&mut t).margins.left, exact);
    assert!(setup(&mut t).grid_lines);
    assert_eq!(undo_len(&mut t), 1);
    // A margin typed is written as typed.
    open(&mut t);
    set(&mut t, "left", Json::Str("0.7087".into()));
    set(&mut t, "left", Json::Str("0.5".into()));
    ok(&mut t);
    assert_eq!(setup(&mut t).margins.left, 0.5);
}

#[test]
fn an_untouched_ok_keeps_definitions_the_fields_cannot_show() {
    let mut t = tab();
    let area_f = r#"INDIRECT("Sheet1!A1:B2")"#;
    let titles_f = "Sheet1!$1:$1,Sheet1!$A:$A";
    {
        let wb = &mut view(&mut t).pkg.workbook;
        for (name, formula) in [(area::PRINT_AREA, area_f), (area::PRINT_TITLES, titles_f)] {
            wb.defined_names.push(gridcore::sheet::DefinedName {
                name: name.into(),
                scope: Some(0),
                formula: formula.into(),
            });
        }
    }
    open(&mut t);
    // The dialog cannot spell INDIRECT, so it shows no print area.
    assert_eq!(text(t.dialogs.top().unwrap(), "print-area"), "");
    ok(&mut t);
    assert_eq!(defined(&mut t, area::PRINT_AREA).as_deref(), Some(area_f));
    assert_eq!(
        defined(&mut t, area::PRINT_TITLES).as_deref(),
        Some(titles_f)
    );
    assert_eq!(undo_len(&mut t), 0);
    // Nor does an unrelated change touch them.
    open(&mut t);
    set(&mut t, "orientation", Json::Str("Landscape".into()));
    ok(&mut t);
    assert!(setup(&mut t).orientation.is_landscape());
    assert_eq!(defined(&mut t, area::PRINT_AREA).as_deref(), Some(area_f));
    assert_eq!(
        defined(&mut t, area::PRINT_TITLES).as_deref(),
        Some(titles_f)
    );
    // Typing an area is a change: it replaces the definition.
    open(&mut t);
    set(&mut t, "print-area", Json::Str("A1:B2".into()));
    ok(&mut t);
    assert_eq!(print_area(&mut t), [(0, 0, 1, 1)]);
    assert_eq!(
        defined(&mut t, area::PRINT_TITLES).as_deref(),
        Some(titles_f)
    );
}

#[test]
fn references_name_this_sheet_or_none() {
    let mut t = tab();
    view(&mut t).pkg.workbook.sheets[0].name = "Sales, East".into();
    // Another sheet's range is refused, not applied here.
    for (control, value) in [
        ("print-area", "Other!$A$1:$C$10"),
        ("title-rows", "Other!$1:$1"),
    ] {
        open(&mut t);
        set(&mut t, control, Json::Str(value.into()));
        let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
        assert!(
            err.contains("is on sheet 'Other', not on 'Sales, East'"),
            "{err}"
        );
        assert_eq!(undo_len(&mut t), 0);
        crate::dialog_host::dialog_click(&mut t, "Cancel").unwrap();
    }
    // This sheet's name, quoted with a comma in it, and two areas.
    open(&mut t);
    set(
        &mut t,
        "print-area",
        Json::Str("'Sales, East'!$A$1:$B$2, D4".into()),
    );
    set(
        &mut t,
        "title-rows",
        Json::Str("'sales, east'!$1:$2".into()),
    );
    ok(&mut t);
    assert_eq!(print_area(&mut t), [(0, 0, 1, 1), (3, 3, 3, 3)]);
    let v = view(&mut t);
    assert_eq!(area::print_titles(&v.pkg.workbook, 0).rows, Some((0, 1)));
}

#[test]
fn the_dialog_opens_on_the_sheets_setup() {
    let mut t = tab();
    select(&mut t, 0, 0, 2, 1);
    run(&mut t, PageAct::Area(AreaOp::Set));
    run(&mut t, PageAct::Landscape(true));
    run(&mut t, PageAct::Margins(MarginPreset::Wide));
    open(&mut t);
    let d = t.dialogs.top().unwrap();
    assert_eq!(choice(d, "orientation"), Some(1));
    assert_eq!(text(d, "scale"), "100");
    assert_eq!(text(d, "left"), "1");
    assert_eq!(text(d, "header"), "0.5");
    assert_eq!(text(d, "print-area"), "A1:B3");
    assert_eq!(choice(d, "paper"), Some(0));
    assert_eq!(text(d, "title-rows"), "");
}

#[test]
fn the_dialogs_ok_applies_every_field_as_one_undo_step() {
    let mut t = tab();
    open(&mut t);
    set(&mut t, "orientation", Json::Str("Landscape".into()));
    set(&mut t, "paper", Json::Str("A4".into()));
    set(&mut t, "scaling", Json::Str("Fit to".into()));
    set(&mut t, "fit-width", Json::Str("1".into()));
    set(&mut t, "fit-height", Json::Str("".into()));
    set(&mut t, "top", Json::Str("1.25".into()));
    set(&mut t, "h-centered", Json::Bool(true));
    set(&mut t, "print-area", Json::Str("A1:D20".into()));
    set(&mut t, "title-rows", Json::Str("$1:$2".into()));
    set(&mut t, "title-cols", Json::Str("A:A".into()));
    set(&mut t, "headings", Json::Bool(true));
    ok(&mut t);
    let p = setup(&mut t);
    assert!(p.orientation.is_landscape());
    assert_eq!(p.paper_size, 9);
    assert_eq!(shown_fit(&p), (1, 0));
    assert_eq!(p.margins.top, 1.25);
    assert!(p.h_centered && p.headings && !p.grid_lines);
    assert_eq!(print_area(&mut t), [(0, 0, 19, 3)]);
    let v = view(&mut t);
    let titles = area::print_titles(&v.pkg.workbook, v.active);
    assert_eq!(titles.rows, Some((0, 1)));
    assert_eq!(titles.cols, Some((0, 0)));
    assert_eq!(undo_len(&mut t), 1);
    assert!(t.dirty);
    // An untouched OK is no step.
    open(&mut t);
    crate::dialog_host::dialog_click(&mut t, "OK").unwrap();
    assert_eq!(undo_len(&mut t), 1);
    // Undo takes the whole dialog back.
    assert!(view(&mut t).undo_step());
    assert_eq!(setup(&mut t), PageSetup::default());
    assert!(print_area(&mut t).is_empty());
}

#[test]
fn the_dialog_refuses_what_excel_refuses_and_changes_nothing() {
    for (control, value, message) in [
        ("scale", "5", "scale 5 is outside 10–400"),
        ("scale", "401", "scale 401 is outside 10–400"),
        (
            "scale",
            "12.5",
            "Adjust to must be a whole number, not '12.5'",
        ),
        (
            "left",
            "-1",
            "left margin -1 must be a number of inches ≥ 0",
        ),
        (
            "title-rows",
            "A1",
            "Rows to repeat at top must be whole rows like $1:$2, not 'A1'",
        ),
        (
            "title-cols",
            "$1:$1",
            "Columns to repeat at left must be whole columns like $A:$B, not '$1:$1'",
        ),
        (
            "print-area",
            "A1:B2,nowhere",
            "Print area 'A1:B2,nowhere' is not a reference",
        ),
    ] {
        let mut t = tab();
        open(&mut t);
        set(&mut t, control, Json::Str(value.into()));
        let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
        assert_eq!(err, message, "{control} = {value}");
        assert_eq!(t.dialogs.top().map(|d| d.id.as_str()), Some("page-setup"));
        assert_eq!(undo_len(&mut t), 0);
        assert!(!t.dirty);
        assert_eq!(setup(&mut t), PageSetup::default());
    }
}

#[test]
fn cancel_closes_the_dialog_and_changes_nothing() {
    let mut t = tab();
    open(&mut t);
    set(&mut t, "orientation", Json::Str("Landscape".into()));
    crate::dialog_host::dialog_click(&mut t, "Cancel").unwrap();
    assert!(t.dialogs.top().is_none());
    assert_eq!(setup(&mut t).orientation, Orientation::Default);
    assert_eq!(undo_len(&mut t), 0);
}

// ---- the file --------------------------------------------------------------

#[test]
fn page_layout_edits_survive_save_and_reload() {
    let mut t = tab();
    select(&mut t, 0, 0, 9, 3);
    run(&mut t, PageAct::Area(AreaOp::Set));
    select(&mut t, 5, 0, 5, 0);
    run(&mut t, PageAct::Break(BreakOp::Insert));
    run(&mut t, PageAct::Landscape(true));
    run(&mut t, PageAct::Margins(MarginPreset::Narrow));
    run(&mut t, PageAct::Paper(9));
    run(&mut t, PageAct::FitWidth(1));
    run(&mut t, PageAct::PrintGridlines);
    run(&mut t, PageAct::PrintHeadings);
    let (bytes, _) = crate::sheet_bytes(view(&mut t), None);
    let pkg = gridcore::xlsx::load_xlsx(&bytes).unwrap();
    let wb = &pkg.workbook;
    let p = &wb.sheets[0].page_setup;
    assert!(p.orientation.is_landscape());
    assert_eq!(p.margins, MarginPreset::Narrow.margins());
    assert_eq!(p.paper_size, 9);
    assert_eq!(shown_fit(p), (1, 0));
    assert!(p.grid_lines && p.headings);
    assert_eq!(area::print_area(wb, 0), [(0, 0, 9, 3)]);
    assert_eq!(area::manual_breaks(&wb.sheets[0]), (vec![5], vec![]));
}

#[test]
fn add_to_print_area_refuses_a_definition_that_is_not_a_list_of_ranges() {
    let mut t = tab();
    let f = "OFFSET(Sheet1!$A$1,0,0,5,3)";
    view(&mut t)
        .pkg
        .workbook
        .defined_names
        .push(gridcore::sheet::DefinedName {
            name: area::PRINT_AREA.into(),
            scope: Some(0),
            formula: f.into(),
        });
    select(&mut t, 9, 0, 9, 1);
    run(&mut t, PageAct::Area(AreaOp::Add));
    assert_eq!(&*t.status, ADD_REFUSED);
    assert_eq!(defined(&mut t, area::PRINT_AREA).as_deref(), Some(f));
    assert_eq!(undo_len(&mut t), 0);
    assert!(!t.dirty);
    // Set Print Area replaces it.
    run(&mut t, PageAct::Area(AreaOp::Set));
    assert_eq!(print_area(&mut t), [(9, 0, 9, 1)]);
    // A list of ranges, as Excel writes it, takes an addition.
    select(&mut t, 0, 0, 0, 0);
    run(&mut t, PageAct::Area(AreaOp::Add));
    assert_eq!(print_area(&mut t), [(9, 0, 9, 1), (0, 0, 0, 0)]);
}

#[test]
fn the_dialog_refuses_row_zero_without_panicking() {
    let mut t = tab();
    open(&mut t);
    set(&mut t, "title-rows", Json::Str("$0:$1".into()));
    let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
    assert_eq!(err, "Rows to repeat at top '$0:$1' is not a reference");
    assert_eq!(undo_len(&mut t), 0);
}
