use super::*;
use std::collections::BTreeSet;

const GRID: [Field; 7] = [
    Field::TaskMode,
    Field::Name,
    Field::Duration,
    Field::Start,
    Field::Finish,
    Field::Predecessors,
    Field::ResourceNames,
];

/// A (2d) -> B (1d), C (1d) unlinked; all automatic.
fn plan() -> Editor {
    let task = |uid: i32, name: &str, days: i64| Task {
        uid,
        id: uid,
        name: name.into(),
        outline_level: 1,
        duration_min: days * 480,
        ..Task::default()
    };
    let mut b = task(2, "B", 1);
    b.predecessors.push(Predecessor::fs(1));
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![task(1, "A", 2), b, task(3, "C", 1)],
        ..Project::default()
    })
}

fn cells(ed: &Editor) -> Vec<(i32, &'static str)> {
    ed.changed_cells(&GRID)
        .into_iter()
        .map(|(uid, f)| {
            let name = match f {
                Field::Duration => "Duration",
                Field::Start => "Start",
                Field::Finish => "Finish",
                Field::Name => "Name",
                Field::Predecessors => "Predecessors",
                _ => "other",
            };
            (uid, name)
        })
        .collect()
}

const DURATION_EDIT: [(i32, &str); 4] =
    [(1, "Duration"), (1, "Finish"), (2, "Start"), (2, "Finish")];

#[test]
fn a_new_editor_has_no_changed_cells() {
    let ed = plan();
    assert!(!ed.has_last_change());
    assert!(cells(&ed).is_empty());
}

#[test]
fn a_duration_edit_changes_its_task_and_the_successor_not_the_name() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    assert_eq!(cells(&ed), DURATION_EDIT);
    // The next change replaces the record.
    ed.rename(3, "Cee").unwrap();
    assert_eq!(cells(&ed), [(3, "Name")]);
}

#[test]
fn undo_and_redo_are_changes_and_report_the_same_cells() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    ed.rename(3, "Cee").unwrap();
    assert!(ed.undo());
    assert_eq!(cells(&ed), [(3, "Name")]);
    assert!(ed.undo());
    assert_eq!(cells(&ed), DURATION_EDIT);
    assert!(ed.redo());
    assert_eq!(cells(&ed), DURATION_EDIT);
}

#[test]
fn a_view_change_keeps_the_record() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    ed.set_sort(Some(Sort {
        field: Field::Name,
        descending: true,
    }));
    ed.set_filter(
        Field::Name,
        Some(BTreeSet::from(["A".to_string(), "B".to_string()])),
    );
    ed.set_highlight(Field::Name, Some(BTreeSet::from(["A".to_string()])));
    ed.select(2);
    assert_eq!(cells(&ed), DURATION_EDIT);
}

/// v2 point 1: a text style or a column width is no change of a cell, so
/// the Text Styles OK that styles `Changed Cells` does not clear them.
#[test]
fn a_formatting_or_width_change_keeps_the_record() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    ed.edit_formatting(|f| {
        f.set_category(
            "Changed Cells",
            crate::formatting::TextStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
    });
    assert_eq!(cells(&ed), DURATION_EDIT);
    ed.edit_table_view(|v| v.set_width("Name", 200.));
    assert_eq!(cells(&ed), DURATION_EDIT);
    // Undoing them is no change of a cell either.
    assert!(ed.undo() && ed.undo());
    assert_eq!(cells(&ed), DURATION_EDIT);
    // A batch that styles and edits is a change.
    ed.batch(|ed| {
        ed.edit_formatting(|f| f.text_styles.clear());
        ed.rename(3, "Cee")
    })
    .unwrap();
    assert_eq!(cells(&ed), [(3, "Name")]);
}

#[test]
fn a_batch_is_one_change_and_a_rolled_back_one_keeps_the_record() {
    let mut ed = plan();
    ed.rename(3, "Cee").unwrap();
    ed.batch(|ed| {
        ed.set_duration(1, "3d")?;
        ed.rename(1, "Ay")
    })
    .unwrap();
    let mut both = DURATION_EDIT.to_vec();
    both.insert(0, (1, "Name"));
    assert_eq!(cells(&ed), both);
    let rev = ed.revision();
    let refused: Result<(), String> = ed.batch(|ed| {
        ed.rename(2, "Bee")?;
        Err("no".into())
    });
    assert!(refused.is_err());
    assert_eq!(ed.revision(), rev);
    assert_eq!(cells(&ed), both);
}

/// A task the change made has no changed cells (it has no before).
#[test]
fn a_new_task_has_no_changed_cells() {
    let mut ed = plan();
    ed.add_task(None, "New", 480, false).unwrap();
    assert!(ed.changed_cells(&GRID).is_empty(), "{:?}", cells(&ed));
    // Inserted above B: the new task has none, though B's cells change
    // (the insert links the new task in before it).
    let mut ed = plan();
    ed.add_task(Some(1), "New", 480, false).unwrap();
    let new_uid = ed.project().tasks[1].uid;
    let changed = ed.changed_cells(&GRID);
    assert!(
        changed.iter().all(|(uid, _)| *uid != new_uid),
        "{changed:?}"
    );
    assert!(changed.contains(&(2, Field::Predecessors)), "{changed:?}");
}

#[test]
fn a_replaced_project_has_none() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    ed.replace_project(plan().project().clone());
    assert!(!ed.has_last_change());
    assert!(cells(&ed).is_empty());
}

#[test]
fn a_save_keeps_the_record() {
    let mut ed = plan();
    ed.set_duration(1, "3d").unwrap();
    ed.commit_save();
    assert_eq!(cells(&ed), DURATION_EDIT);
}
