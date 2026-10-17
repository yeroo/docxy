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
    assert_eq!(column_kind(COL_NAME), ColumnKind::Text);
    assert_eq!(column_kind(COL_ID), ColumnKind::Number);
    assert_eq!(column_kind(COL_DURATION), ColumnKind::Number);
    assert_eq!(column_kind(COL_FINISH), ColumnKind::Date);
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
    assert_eq!(t.status.as_ref(), AUTO_FILTER_STATUS);

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
    assert!(plain.contains(r#""Name":240"#), "{plain}");
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
