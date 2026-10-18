use super::*;
use core::prelude::v1::test;

fn item(text: &str, date: Option<(i64, u32, u32)>) -> ValueItem {
    ValueItem {
        text: text.into(),
        date,
    }
}

#[test]
fn sort_words_follow_the_columns_type() {
    assert_eq!(column_kind(Field::Name), ColumnKind::Text);
    assert_eq!(column_kind(Field::Id), ColumnKind::Number);
    assert_eq!(column_kind(Field::Duration), ColumnKind::Number);
    assert_eq!(column_kind(Field::Finish), ColumnKind::Date);
    assert_eq!(
        sort_labels(ColumnKind::Text),
        ["Sort A to Z", "Sort Z to A"]
    );
    assert_eq!(
        sort_labels(ColumnKind::Date),
        ["Sort Earliest to Latest", "Sort Latest to Earliest"]
    );
    assert_eq!(
        sort_labels(ColumnKind::Number),
        ["Sort Smallest to Largest", "Sort Largest to Smallest"]
    );
}

#[test]
fn dates_form_a_year_month_day_tree_and_text_a_flat_list() {
    let tree = build_tree(
        &[
            item("Mon 3/2/26", Some((2026, 3, 2))),
            item("Tue 3/3/26", Some((2026, 3, 3))),
            item("Wed 4/1/26", Some((2026, 4, 1))),
            item("Fri 1/1/27", Some((2027, 1, 1))),
            item("NA", None),
        ],
        true,
    );
    assert_eq!(
        tree.labels,
        [
            "(Select All)",
            "2026",
            "March",
            "2",
            "3",
            "April",
            "1",
            "2027",
            "January",
            "1",
            "NA"
        ]
    );
    assert_eq!(tree.depths, [0, 1, 2, 3, 3, 2, 3, 1, 2, 3, 1]);
    assert_eq!(tree.leaf_count(), 5);
    let flat = build_tree(&[item("", None), item("A", None)], false);
    assert_eq!(flat.labels, ["(Select All)", "(Blanks)", "A"]);
    assert_eq!(
        flat.checked_texts(&[true, true, false]),
        BTreeSet::from(["".into()])
    );
}

#[test]
fn boxes_follow_the_allowed_texts_and_parents_are_checked_when_all_are() {
    let tree = build_tree(
        &[
            item("a", Some((2026, 3, 2))),
            item("b", Some((2026, 3, 3))),
            item("c", Some((2026, 4, 1))),
        ],
        true,
    );
    // Unfiltered: everything on.
    assert!(tree_checks(&tree, None).iter().all(|&c| c));
    // March only: the March heading and its days on, the rest off.
    let march = BTreeSet::from(["a".to_string(), "b".to_string()]);
    assert_eq!(
        tree_checks(&tree, Some(&march)),
        [false, false, true, true, true, false, false]
    );
    // One day: its month is not whole.
    let one = BTreeSet::from(["a".to_string()]);
    assert_eq!(
        tree_checks(&tree, Some(&one)),
        [false, false, false, true, false, false, false]
    );
}

// ---- the drop-down on a plan --------------------------------------------

use crate::dialog_host::dialog_click;
use projcore::DateTime;

fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!("project")
    };
    v
}

/// B, A, C under a summary S; manual starts in Jan, Mar and Mar.
fn tab() -> DocTab {
    let task = |uid: i32, name: &str, level: u32, (m, d): (u32, u32)| Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: level,
        duration_min: 480,
        manual: true,
        manual_start: Some(DateTime::from_ymd_hm(2026, m, d, 8, 0)),
        ..Task::default()
    };
    let project = Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            task(1, "S", 1, (1, 6)),
            task(2, "B", 2, (1, 6)),
            task(3, "A", 2, (3, 2)),
            task(4, "C", 2, (3, 9)),
        ],
        ..Project::default()
    };
    let mut t = new_project_tab("Project1");
    t.surface = Surface::Project(ProjectView::new(project, false));
    t
}

fn names(t: &DocTab) -> Vec<String> {
    let ed = &v(t).ed;
    ed.display_rows()
        .into_iter()
        .map(|i| ed.project().tasks[i].name.clone())
        .collect()
}

fn open(t: &mut DocTab, col: usize) {
    let d = menu_dialog(v(t), col);
    t.dialogs.push(d);
}

fn button<'a>(t: &'a DocTab, label: &str) -> &'a crate::dialog::Button {
    t.dialogs
        .top()
        .unwrap()
        .buttons
        .iter()
        .find(|b| b.label == label)
        .unwrap_or_else(|| panic!("no button {label}"))
}

fn set_checks(t: &mut DocTab, edit: impl FnOnce(&mut Vec<bool>, &[u8])) {
    let d = t.dialogs.top_dialog_mut().unwrap();
    let c = d.controls.iter_mut().find(|c| c.name == "values").unwrap();
    let depths = c.depths.clone();
    let Value::Checks(checks) = &mut c.value else {
        panic!("checks")
    };
    edit(checks, &depths);
}

#[test]
fn the_menu_lists_sort_disabled_group_and_filters_and_clear_only_while_filtered() {
    let mut t = tab();
    open(&mut t, COL_FINISH);
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.title, "Filter: Finish");
    let labels: Vec<_> = d.buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Sort Earliest to Latest",
            "Sort Latest to Earliest",
            "Group by",
            "Filters",
            "Clear Filter from Finish",
            "OK",
            "Cancel"
        ]
    );
    assert!(!button(&t, "Group by").enabled && !button(&t, "Filters").enabled);
    assert!(!button(&t, "Clear Filter from Finish").enabled);
    assert!(button(&t, "Sort Latest to Earliest").enabled);

    // Filtered, the same column's Clear is on.
    t.dialogs.pop();
    let allowed = BTreeSet::from(["B".to_string()]);
    let Surface::Project(pv) = &mut t.surface else {
        panic!()
    };
    pv.ed
        .set_filter(projcore::editor::Field::Name, Some(allowed));
    open(&mut t, COL_NAME);
    assert!(button(&t, "Clear Filter from Name").enabled);
}

#[test]
fn sort_latest_first_orders_the_rows_and_closes_the_menu() {
    let mut t = tab();
    open(&mut t, COL_FINISH);
    dialog_click(&mut t, "Sort Latest to Earliest").unwrap();
    assert!(!t.dialogs.is_open());
    assert_eq!(names(&t), ["S", "C", "A", "B"]);
    assert_eq!(t.status.as_ref(), "Latest to Earliest by Finish");
    open(&mut t, COL_NAME);
    dialog_click(&mut t, "Sort A to Z").unwrap();
    assert_eq!(names(&t), ["S", "A", "B", "C"]);
}

#[test]
fn filtering_a_month_keeps_its_rows_and_the_status_says_so() {
    let mut t = tab();
    open(&mut t, COL_FINISH);
    let tree_dates = {
        let d = t.dialogs.top().unwrap();
        d.controls[0].items.clone()
    };
    // (Select All), 2026, January, 6, March, 2, 3 ... as the days finish.
    assert_eq!(&tree_dates[..3], ["(Select All)", "2026", "January"]);
    assert!(tree_dates.contains(&"March".to_string()));
    set_checks(&mut t, |checks, depths| {
        let march = tree_dates.iter().position(|l| l == "March").unwrap();
        gridcore::filter::check_tree(checks, depths, 0, false);
        gridcore::filter::check_tree(checks, depths, march, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    // The summary stays for its March children; January's B goes.
    assert_eq!(names(&t), ["S", "A", "C"]);
    assert!(status_items(&t, false).contains(&("filter", AUTO_FILTER_STATUS.to_string())));
    assert_eq!(t.status.as_ref(), "Filtered by Finish");

    // Reopened, the March days are the checked ones.
    open(&mut t, COL_FINISH);
    let d = t.dialogs.top().unwrap();
    let Value::Checks(checks) = &d.controls[0].value else {
        panic!()
    };
    let on: Vec<_> = d.controls[0]
        .items
        .iter()
        .zip(checks)
        .filter(|(_, c)| **c)
        .map(|(l, _)| l.as_str())
        .collect();
    assert_eq!(on, ["March", "2", "9"]);
}

#[test]
fn filters_on_two_columns_and_and_clear_filter_restores_the_rows() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        // Items: (Select All), A, B, C, S. Uncheck C.
        gridcore::filter::check_tree(checks, depths, 3, false);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(names(&t), ["S", "B", "A"]);

    open(&mut t, COL_FINISH);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
        // January's day only.
        gridcore::filter::check_tree(checks, depths, 3, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(names(&t), ["S", "B"]);
    assert_eq!(v(&t).ed.filtered_fields().len(), 2);

    open(&mut t, COL_NAME);
    dialog_click(&mut t, "Clear Filter from Name").unwrap();
    assert!(!t.dialogs.is_open());
    assert_eq!(names(&t), ["S", "B"], "the Finish filter still applies");
    open(&mut t, COL_FINISH);
    dialog_click(&mut t, "Clear Filter from Finish").unwrap();
    assert_eq!(names(&t), ["S", "B", "A", "C"]);
    assert!(
        !status_items(&t, false)
            .iter()
            .any(|(id, _)| *id == "filter")
    );
}

#[test]
fn ok_with_everything_checked_is_no_filter_and_with_nothing_is_refused() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
    });
    assert_eq!(
        dialog_click(&mut t, "OK"),
        Err("Select at least one value".to_string())
    );
    assert!(t.dialogs.is_open(), "the drop-down stays open");
    assert!(!v(&t).ed.is_filtered());
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert!(!v(&t).ed.is_filtered());
    // Cancel discards a half-made list.
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 1, false);
    });
    dialog_click(&mut t, "Cancel").unwrap();
    assert!(!v(&t).ed.is_filtered());
}

#[test]
fn rows_report_the_live_filter_sort_and_widths() {
    let mut t = tab();
    let json = |t: &DocTab| rows_json(&v(t).ed).to_string();
    let plain = json(&t);
    assert!(plain.contains(r#""filter":"All Tasks""#), "{plain}");
    assert!(plain.contains(r#""sort":"ID""#), "{plain}");
    assert!(plain.contains(r#""Name":160"#), "{plain}");
    open(&mut t, COL_NAME);
    dialog_click(&mut t, "Sort Z to A").unwrap();
    let Surface::Project(pv) = &mut t.surface else {
        panic!()
    };
    pv.set_column_width(COL_NAME, 300.);
    pv.ed.set_filter(
        projcore::editor::Field::Name,
        Some(BTreeSet::from(["A".to_string()])),
    );
    let after = json(&t);
    assert!(after.contains(r#""filter":"AutoFilter""#), "{after}");
    assert!(after.contains(r#""sort":"Name""#), "{after}");
    assert!(after.contains(r#""sort_descending":true"#), "{after}");
    assert!(after.contains(r#""Name":300"#), "{after}");
    assert!(after.contains(r#""column":"Name""#), "{after}");
    assert!(after.contains(r#""count":2"#), "{after}");
}

/// The `.uit` cases' fixture: Plan, Build, Test, Ship finish in January,
/// March, March and April.
#[test]
fn the_cases_fixture_finishes_in_three_months() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../uiharness/fixtures/project-filter.xml");
    let xml = std::fs::read_to_string(path).unwrap();
    let ed = ProjectEditor::new(projcore::mspdi::read_mspdi(&xml).unwrap());
    let finishes: Vec<_> = ed
        .project()
        .tasks
        .iter()
        .map(|t| project_row(&ed, t)[COL_FINISH].clone())
        .collect();
    assert_eq!(
        finishes,
        ["Mon 1/12/26", "Mon 3/2/26", "Tue 3/17/26", "Mon 4/6/26"]
    );
}

fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!("project")
    };
    v
}

/// Type `name` into the entry row's Name cell and press `key`.
fn type_entry(t: &mut DocTab, name: &str, key: &str) {
    vm(t).enter_entry_row();
    vm(t).col = COL_NAME;
    vm(t).open_cell(Some(name)).unwrap();
    project_cell_input(t, key, None, Modifiers::default());
}

#[test]
fn enter_after_an_entry_row_commit_returns_to_the_entry_row_under_a_sort() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    dialog_click(&mut t, "Sort A to Z").unwrap();
    // "Amy" ranks between A and B, so it is not the last displayed row.
    type_entry(&mut t, "Amy", "enter");
    assert!(v(&t).on_entry_row(), "the cursor is on the entry row");
    assert_eq!(names(&t), ["S", "A", "Amy", "B", "C"]);
    assert!(v(&t).cell.is_none());
    // The next name is a new task, not a rewrite of Amy.
    type_entry(&mut t, "Bob", "enter");
    assert_eq!(names(&t), ["S", "A", "Amy", "B", "Bob", "C"]);
    assert!(v(&t).on_entry_row());
}

#[test]
fn a_new_entry_task_a_filter_hides_leaves_the_cursor_on_the_entry_row() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
        gridcore::filter::check_tree(checks, depths, 2, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(names(&t), ["S", "B"]);
    type_entry(&mut t, "Zed", "enter");
    assert_eq!(v(&t).ed.project().tasks.len(), 5);
    assert!(v(&t).on_entry_row());
    assert_eq!(names(&t), ["S", "B"], "Zed fails the Name filter");
    assert!(v(&t).ed.display_rows().contains(&v(&t).ed.sel()));
}

#[test]
fn an_edit_that_hides_the_cursor_row_moves_it_to_a_shown_row() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
        gridcore::filter::check_tree(checks, depths, 1, true);
        gridcore::filter::check_tree(checks, depths, 2, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(names(&t), ["S", "B", "A"]);
    vm(&mut t).ed.select(2);
    vm(&mut t).ed.rename(3, "Zed").unwrap();
    let ed = &v(&t).ed;
    assert!(
        ed.display_rows().contains(&ed.sel()),
        "the cursor row is shown"
    );
    assert_eq!(
        v(&t).display_row(),
        v(&t)
            .ed
            .display_rows()
            .iter()
            .position(|&i| i == v(&t).ed.sel())
            .unwrap()
    );
}

#[test]
fn select_all_on_a_filtered_column_clears_the_filter() {
    let mut t = tab();
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
        gridcore::filter::check_tree(checks, depths, 2, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert!(v(&t).ed.is_filtered());
    open(&mut t, COL_NAME);
    set_checks(&mut t, |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, true);
    });
    dialog_click(&mut t, "OK").unwrap();
    assert!(!v(&t).ed.is_filtered());
    assert_eq!(t.status.as_ref(), "Filter cleared from Name");
    assert_eq!(names(&t), ["S", "B", "A", "C"]);
}

/// Names A..E (levels 1), Finish all on different March days.
fn five() -> DocTab {
    let task = |uid: i32, name: &str, day: u32| Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: 1,
        duration_min: 480,
        manual: true,
        manual_start: Some(DateTime::from_ymd_hm(2026, 3, day, 8, 0)),
        ..Task::default()
    };
    let project = Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            task(1, "A", 2),
            task(2, "B", 3),
            task(3, "C", 4),
            task(4, "D", 5),
            task(5, "E", 6),
        ],
        ..Project::default()
    };
    let mut t = new_project_tab("Project1");
    t.surface = Surface::Project(ProjectView::new(project, false));
    t
}

fn filter_names(t: &mut DocTab, keep: &[usize]) {
    open(t, COL_NAME);
    let keep = keep.to_vec();
    set_checks(t, move |checks, depths| {
        gridcore::filter::check_tree(checks, depths, 0, false);
        for k in keep {
            gridcore::filter::check_tree(checks, depths, k, true);
        }
    });
    dialog_click(t, "OK").unwrap();
}

#[test]
fn a_commit_that_hides_the_edited_row_leaves_the_cursor_on_the_row_that_followed() {
    let mut t = five();
    filter_names(&mut t, &[1, 3, 5, 2]); // A, C, E, B shown; D hidden
    assert_eq!(names(&t), ["A", "B", "C", "E"]);
    // Rename C so it drops out; Enter must not skip E or jump to the top.
    vm(&mut t).ed.select(2);
    vm(&mut t).col = COL_NAME;
    vm(&mut t).open_cell(Some("Zed")).unwrap();
    project_cell_input(&mut t, "enter", None, Modifiers::default());
    assert_eq!(names(&t), ["A", "B", "E"]);
    let ed = &v(&t).ed;
    assert_eq!(
        ed.project().tasks[ed.sel()].name,
        "E",
        "the row that took its place"
    );
    assert_eq!(v(&t).col, COL_NAME);
    // Tab does not carry the cursor across into another task's next column.
    vm(&mut t).ed.select(1);
    vm(&mut t).open_cell(Some("Yak")).unwrap();
    project_cell_input(&mut t, "tab", None, Modifiers::default());
    assert_eq!(v(&t).col, COL_NAME);
    let ed = &v(&t).ed;
    assert_eq!(ed.project().tasks[ed.sel()].name, "E");
    // Shift+Enter goes up from the row that took its place.
    vm(&mut t).open_cell(Some("Xi")).unwrap();
    project_cell_input(
        &mut t,
        "enter",
        None,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(names(&t), ["A"]);
    let ed = &v(&t).ed;
    assert_eq!(ed.project().tasks[ed.sel()].name, "A");
}

#[test]
fn add_task_under_a_filter_that_hides_it_puts_the_cursor_on_the_entry_row() {
    let mut t = five();
    filter_names(&mut t, &[1]);
    assert_eq!(names(&t), ["A"]);
    vm(&mut t).ed.select(0);
    apply_project_act(&mut t, ProjectAct::AddTask);
    assert_eq!(v(&t).ed.project().tasks.len(), 6);
    assert!(v(&t).on_entry_row());
    assert!(v(&t).ed.display_rows().contains(&v(&t).ed.sel()));
    assert_eq!(names(&t), ["A"]);
}

#[test]
fn an_edit_that_hides_every_row_leaves_the_cursor_on_the_entry_row() {
    let mut t = five();
    filter_names(&mut t, &[1]);
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    vm(&mut t).open_cell(Some("Zed")).unwrap();
    project_cell_input(&mut t, "tab", None, Modifiers::default());
    assert!(names(&t).is_empty());
    assert!(v(&t).on_entry_row());
    // Up from the entry row with nothing drawn does nothing.
    assert!(vm(&mut t).key("up", false));
    assert!(v(&t).on_entry_row());
}

#[test]
fn delete_under_a_filter_or_sort_puts_the_cursor_on_the_next_drawn_row() {
    let mut t = five();
    filter_names(&mut t, &[1, 3, 5]); // A, C, E
    vm(&mut t).ed.select(2);
    vm(&mut t).col = COL_ID; // Delete on the ID column deletes the task
    apply_project_act(&mut t, ProjectAct::ClearCell);
    let ed = &v(&t).ed;
    assert_eq!(ed.project().tasks.len(), 4);
    assert!(
        ed.project().tasks.iter().all(|x| x.name != "C"),
        "C is gone"
    );
    assert_eq!(ed.project().tasks[ed.sel()].name, "E");
    // Sorted Z to A (E, D, C, B, A minus nothing filtered): delete the
    // middle one and the next drawn row is the cursor.
    let mut t = five();
    open(&mut t, COL_NAME);
    dialog_click(&mut t, "Sort Z to A").unwrap();
    assert_eq!(names(&t), ["E", "D", "C", "B", "A"]);
    vm(&mut t).ed.select(2); // C
    vm(&mut t).col = COL_ID;
    apply_project_act(&mut t, ProjectAct::ClearCell);
    let ed = &v(&t).ed;
    assert_eq!(ed.project().tasks.len(), 4);
    assert_eq!(ed.project().tasks[ed.sel()].name, "B");
}

#[test]
fn hiding_the_last_drawn_row_goes_to_the_entry_row_on_enter_and_stays_up_on_shift_enter() {
    let mut t = five();
    filter_names(&mut t, &[1, 2, 3]); // A, B, C
    let rename_last = |t: &mut DocTab, key: &str, shift: bool| {
        vm(t).ed.select(2);
        vm(t).col = COL_NAME;
        vm(t).open_cell(Some("Zed")).unwrap();
        project_cell_input(
            t,
            key,
            None,
            Modifiers {
                shift,
                ..Modifiers::default()
            },
        );
    };
    rename_last(&mut t, "enter", true);
    let ed = &v(&t).ed;
    assert_eq!(
        ed.project().tasks[ed.sel()].name,
        "B",
        "not A: no second step up"
    );
    assert!(!v(&t).on_entry_row());
    // Undo, then Enter: the table is left for the entry row.
    assert!(vm(&mut t).ed.undo());
    vm(&mut t).ed.select(2);
    rename_last(&mut t, "enter", false);
    assert!(v(&t).on_entry_row());
}

#[test]
fn find_under_a_filter_only_finds_drawn_rows() {
    let mut t = five();
    filter_names(&mut t, &[1, 2]); // A, B
    assert!(vm(&mut t).find("d").is_some());
    assert_eq!(names(&t), ["A", "B"]);
    let ed = &v(&t).ed;
    assert!(ed.display_rows().contains(&ed.sel()));
}

fn rename_and_press(t: &mut DocTab, index: usize, key: &str, shift: bool) {
    vm(t).ed.select(index);
    vm(t).col = COL_NAME;
    vm(t).open_cell(Some("Zed")).unwrap();
    project_cell_input(
        t,
        key,
        None,
        Modifiers {
            shift,
            ..Modifiers::default()
        },
    );
}

#[test]
fn a_trailing_blank_row_does_not_count_as_a_row_that_followed() {
    for (key, shift) in [("enter", false), ("enter", true)] {
        let mut t = five();
        // A blank row after E, which no filter hides.
        let Surface::Project(pv) = &mut t.surface else {
            panic!()
        };
        pv.ed.insert_blank_row(None).unwrap();
        filter_names(&mut t, &[1, 2, 3]); // A, B, C
        assert_eq!(names(&t), ["A", "B", "C", ""]);
        rename_and_press(&mut t, 2, key, shift);
        let ed = &v(&t).ed;
        if shift {
            assert_eq!(ed.project().tasks[ed.sel()].name, "B", "up from C, once");
            assert!(!v(&t).on_entry_row());
        } else {
            assert!(v(&t).on_entry_row(), "no drawn task followed C");
        }
    }
}

#[test]
fn a_task_hidden_by_the_same_edit_does_not_count_as_a_row_that_followed() {
    use projcore::Predecessor;
    let task = |uid: i32, name: &str, days: i64, pred: Option<i32>| Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: 1,
        duration_min: days * 480,
        predecessors: pred.map(Predecessor::fs).into_iter().collect(),
        ..Task::default()
    };
    // D finishes 1/6; A 1/5, B (after A) 1/6, C (after B) 1/7.
    let project = Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            task(4, "D", 2, None),
            task(1, "A", 1, None),
            task(2, "B", 1, Some(1)),
            task(3, "C", 1, Some(2)),
        ],
        ..Project::default()
    };
    for (key, shift) in [("enter", false), ("enter", true)] {
        let mut t = new_project_tab("Project1");
        t.surface = Surface::Project(ProjectView::new(project.clone(), false));
        let wanted = BTreeSet::from(["Tue 1/6/26".to_string(), "Wed 1/7/26".to_string()]);
        vm(&mut t).ed.set_filter(Field::Finish, Some(wanted));
        assert_eq!(names(&t), ["D", "B", "C"]);
        // Lengthen B: it and C (its successor) both leave the view.
        vm(&mut t).ed.select(2);
        vm(&mut t).col = COL_DURATION;
        vm(&mut t).open_cell(Some("5d")).unwrap();
        project_cell_input(
            &mut t,
            key,
            None,
            Modifiers {
                shift,
                ..Modifiers::default()
            },
        );
        assert_eq!(names(&t), ["D"]);
        if shift {
            let ed = &v(&t).ed;
            assert_eq!(ed.project().tasks[ed.sel()].name, "D");
            assert!(!v(&t).on_entry_row());
        } else {
            assert!(v(&t).on_entry_row(), "C went with B: none followed");
        }
    }
}
