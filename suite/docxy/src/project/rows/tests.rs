use super::*;
use core::prelude::v1::test;

/// The harness case's fixture: 1 Outer (L1) / 2 Inner (L2) / 3 Deep (L3) /
/// 4 Sibling (L2) / 5 blank / 6 After (L1).
fn nested() -> ProjectEditor {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/project-rows.xml");
    let xml = std::fs::read_to_string(path).unwrap();
    ProjectEditor::new(mspdi::read_mspdi(&xml).unwrap())
}

fn flat() -> ProjectEditor {
    let mut p = Project::default();
    p.tasks = (1..=3)
        .map(|n| Task {
            uid: n,
            id: n,
            name: format!("Task {n}"),
            outline_level: 1,
            ..Task::default()
        })
        .collect();
    ProjectEditor::new(p)
}

fn listed(ed: &ProjectEditor) -> Json {
    rows_json(ed)
}

fn ids(reply: &Json) -> Vec<i64> {
    reply
        .get("rows")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .map(|r| r.get("id").and_then(Json::as_i64).unwrap())
        .collect()
}

fn row(reply: &Json, n: usize) -> &Json {
    &reply.get("rows").and_then(Json::as_array).unwrap()[n]
}

fn num(reply: &Json, key: &str) -> i64 {
    reply.get(key).and_then(Json::as_i64).unwrap()
}

#[test]
fn a_flat_plan_lists_every_task_in_order_and_not_the_entry_row() {
    let ed = flat();
    let reply = listed(&ed);
    assert_eq!(ids(&reply), [1, 2, 3]);
    assert_eq!((num(&reply, "count"), num(&reply, "total")), (3, 3));
    for key in ["view", "table", "filter", "group", "sort"] {
        assert!(reply.get_str(key).is_some(), "{key}");
    }
    let first = row(&reply, 0);
    assert_eq!(num(first, "row"), 1);
    assert_eq!(first.get_str("kind"), Some("task"));
    assert_eq!(first.get_str("name"), Some("Task 1"));
    assert_eq!(num(first, "level"), 1);
    assert_eq!(first.get("collapsed").and_then(Json::as_bool), Some(false));
    let cells = first.get("cells").and_then(Json::as_array).unwrap();
    assert_eq!(cells.len(), COLUMN_COUNT);
    assert_eq!(cells[COL_NAME].as_str(), Some("Task 1"));
}

#[test]
fn the_nested_fixture_reads_as_summaries_a_blank_and_tasks() {
    let ed = nested();
    let reply = listed(&ed);
    assert_eq!(ids(&reply), [1, 2, 3, 4, 5, 6]);
    let kinds: Vec<_> = (0..6)
        .map(|n| row(&reply, n).get_str("kind").unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        ["summary", "summary", "task", "task", "blank", "task"]
    );
    let blank = row(&reply, 4);
    assert_eq!(num(blank, "uid"), 5);
    assert_eq!(blank.get_str("name"), Some(""));
    assert_eq!(blank.get("level"), Some(&Json::Null));
    assert_eq!(blank.get("blank").and_then(Json::as_bool), Some(true));
    assert_eq!(blank.get("summary").and_then(Json::as_bool), Some(false));
    assert_eq!(num(row(&reply, 2), "level"), 3);
}

#[test]
fn collapsing_hides_subtrees_without_renumbering_and_expanding_restores_them() {
    let mut ed = nested();
    // The inner summary alone hides only its own subtask.
    ed.set_collapsed(2, true).unwrap();
    let reply = listed(&ed);
    assert_eq!(ids(&reply), [1, 2, 4, 5, 6]);
    assert_eq!((num(&reply, "count"), num(&reply, "total")), (5, 6));
    assert_eq!(
        row(&reply, 1).get("collapsed").and_then(Json::as_bool),
        Some(true)
    );
    assert_eq!(
        row(&reply, 0).get("collapsed").and_then(Json::as_bool),
        Some(false)
    );

    // The outer one hides the collapsed inner one too; the blank after the
    // outline stays, and IDs keep their numbers.
    ed.set_collapsed(1, true).unwrap();
    let reply = listed(&ed);
    assert_eq!(ids(&reply), [1, 5, 6]);
    assert_eq!(num(row(&reply, 2), "row"), 3);
    assert_eq!(row(&reply, 2).get_str("name"), Some("After"));

    // Expanding the outer one keeps the inner one collapsed.
    ed.set_collapsed(1, false).unwrap();
    assert_eq!(ids(&listed(&ed)), [1, 2, 4, 5, 6]);
    ed.set_collapsed(2, false).unwrap();
    assert_eq!(ids(&listed(&ed)), [1, 2, 3, 4, 5, 6]);
}

#[test]
fn a_drawn_row_maps_to_its_task_the_entry_row_or_nothing() {
    let mut ed = nested();
    ed.set_collapsed(1, true).unwrap();
    // Drawn: Outer, blank, After, then the entry row.
    assert_eq!(shown_task(&ed, 0), Some(ShownRow::Task(0)));
    assert_eq!(shown_task(&ed, 1), Some(ShownRow::Task(4)));
    assert_eq!(shown_task(&ed, 2), Some(ShownRow::Task(5)));
    assert_eq!(shown_task(&ed, 3), Some(ShownRow::Entry));
    assert_eq!(shown_task(&ed, 4), None);
    assert_eq!(shown_row_of(&ed, 5), Some(2));
    assert_eq!(shown_row_of(&ed, 2), None);

    let empty = ProjectEditor::new(Project::default());
    assert_eq!(shown_task(&empty, 0), Some(ShownRow::Entry));
    assert_eq!(shown_task(&empty, 1), None);
}

fn args(text: &str) -> Json {
    Json::parse(text).unwrap()
}

#[test]
fn a_cell_is_addressed_by_drawn_row_or_by_uid_and_column() {
    let mut ed = nested();
    ed.set_collapsed(1, true).unwrap();
    assert_eq!(
        project_cell_target(&args(r#"{"cell":"C3"}"#), &ed),
        Ok(CellTarget {
            row: Some(2),
            at: Some(ShownRow::Task(5)),
            col: 2
        })
    );
    // A hidden task is reachable by UID, with no drawn row.
    assert_eq!(
        project_cell_target(&args(r#"{"uid":3,"column":"name"}"#), &ed),
        Ok(CellTarget {
            row: None,
            at: Some(ShownRow::Task(2)),
            col: COL_NAME
        })
    );
    assert_eq!(
        project_cell_target(&args(r#"{"uid":6,"column":7}"#), &ed),
        Ok(CellTarget {
            row: Some(2),
            at: Some(ShownRow::Task(5)),
            col: COL_RESOURCES
        })
    );
    assert_eq!(
        project_cell_target(&args(r#"{"uid":6,"column":"Resource Names"}"#), &ed).map(|t| t.col),
        Ok(COL_RESOURCES)
    );
    let refused = |text: &str| project_cell_target(&args(text), &ed).unwrap_err();
    assert!(refused(r#"{"cell":"C1","uid":1,"column":0}"#).contains("not both"));
    assert!(refused(r#"{"uid":1}"#).contains("'column'"));
    assert!(refused(r#"{"uid":99,"column":0}"#).contains("UID 99"));
    assert!(refused(r#"{"uid":1,"column":"Nope"}"#).contains("Resource Names"));
    assert!(refused(r#"{"uid":1,"column":8}"#).contains("No Project column"));
    assert!(refused(r#"{"uid":"1","column":0}"#).contains("'uid'"));
}
