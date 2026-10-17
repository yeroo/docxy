use super::*;
use crate::mspdi::{read_mspdi, write_mspdi};

fn editor() -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            Task {
                uid: 1,
                id: 1,
                name: "S".into(),
                outline_level: 1,
                ..Task::default()
            },
            Task {
                uid: 2,
                id: 2,
                name: "A".into(),
                outline_level: 2,
                duration_min: 480,
                ..Task::default()
            },
        ],
        ..Project::default()
    })
}

fn round_trip(ed: &Editor) -> Project {
    read_mspdi(&write_mspdi(ed.project())).unwrap()
}

#[test]
fn percent_complete_range_unchanged_undo_and_round_trip() {
    let mut ed = editor();
    assert_eq!(ed.set_percent_complete(2, 0), Ok(false));
    assert!(!ed.dirty() && ed.undo_depth() == 0);
    assert!(ed.set_percent_complete(2, 101).is_err());
    assert!(ed.set_percent_complete(2, -1).is_err());
    assert!(
        ed.set_percent_complete(1, 10).is_err(),
        "a summary's rolls up"
    );
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_percent_complete(2, 40), Ok(true));
    assert_eq!(ed.set_percent_complete(2, 40), Ok(false));
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(round_trip(&ed).task(2).unwrap().percent_complete, Some(40));
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().percent_complete, None);
}

#[test]
fn percent_complete_cannot_contradict_the_actuals() {
    let day = DateTime::from_ymd_hm(2026, 1, 5, 8, 0);
    let with = |start, finish| {
        let mut proj = editor().project().clone();
        let task = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
        (task.actual_start, task.actual_finish) = (start, finish);
        Editor::new(proj)
    };
    let mut started = with(Some(day), None);
    assert!(
        started.set_percent_complete(2, 0).is_err(),
        "started, not 0%"
    );
    assert_eq!(started.set_percent_complete(2, 10), Ok(true));
    let mut finished = with(Some(day), Some(day));
    assert!(
        finished.set_percent_complete(2, 50).is_err(),
        "finished: 100%"
    );
    assert_eq!(finished.undo_depth(), 0);
    assert_eq!(finished.set_percent_complete(2, 100), Ok(true));
}

#[test]
fn priority_range_default_and_round_trip() {
    let mut ed = editor();
    assert_eq!(ed.set_priority(2, PRIORITY_DEFAULT), Ok(false));
    assert!(ed.set_priority(2, 1001).is_err());
    assert!(ed.set_priority(2, -1).is_err());
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_priority(2, 1000), Ok(true));
    assert_eq!(round_trip(&ed).task(2).unwrap().priority, Some(1000));
    assert_eq!(ed.set_priority(2, 0), Ok(true));
    assert_eq!(ed.undo_depth(), 2);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().priority, Some(1000));
}

#[test]
fn deadline_sets_clears_and_moves_the_miss_indicator() {
    let mut ed = editor();
    let finish = ed.disp_finish(2).unwrap();
    let early = DateTime::from_ymd_hm(2026, 1, 5, 9, 0);
    assert_eq!(ed.set_deadline(2, None), Ok(false));
    assert_eq!(ed.set_deadline(2, Some(early)), Ok(true));
    let task = ed.project().task(2).unwrap();
    assert!(
        task.misses_deadline(finish),
        "the deadline reaches the model"
    );
    assert_eq!(round_trip(&ed).task(2).unwrap().deadline, Some(early));
    assert_eq!(ed.set_deadline(2, None), Ok(true));
    assert_eq!(ed.project().task(2).unwrap().deadline, None);
    assert_eq!(round_trip(&ed).task(2).unwrap().deadline, None);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2).unwrap().deadline, Some(early));
}

#[test]
fn task_type_and_effort_driven_round_trip_and_undo() {
    let mut ed = editor();
    assert_eq!(ed.set_task_type(2, TaskType::FixedUnits), Ok(false));
    assert_eq!(ed.set_effort_driven(2, false), Ok(false));
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_task_type(2, TaskType::FixedWork), Ok(true));
    assert_eq!(ed.set_effort_driven(2, true), Ok(true));
    let back = round_trip(&ed);
    assert_eq!(back.task(2).unwrap().task_type, Some(TaskType::FixedWork));
    assert_eq!(back.task(2).unwrap().effort_driven, Some(true));
    assert_eq!(ed.undo_depth(), 2);
    assert!(ed.undo() && ed.undo());
    let task = ed.project().task(2).unwrap();
    assert_eq!((task.task_type, task.effort_driven), (None, None));
}

#[test]
fn a_batch_of_property_edits_is_one_undo_step_and_an_error_applies_none() {
    let mut ed = editor();
    ed.batch(|ed| {
        ed.set_percent_complete(2, 25)?;
        ed.set_priority(2, 900)?;
        ed.set_task_type(2, TaskType::FixedDuration)?;
        ed.set_effort_driven(2, true)?;
        ed.set_deadline(2, Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0)))?;
        Ok(())
    })
    .unwrap();
    assert_eq!(ed.undo_depth(), 1);
    assert!(ed.undo());
    assert_eq!(ed.project().task(2), editor().project().task(2));
    assert!(!ed.undo());

    let mut ed = editor();
    let before = ed.project().clone();
    let result = ed.batch(|ed| {
        ed.set_priority(2, 900)?;
        ed.set_percent_complete(2, 500)
    });
    assert!(result.is_err());
    assert_eq!(*ed.project(), before);
    assert_eq!((ed.undo_depth(), ed.dirty()), (0, false));
}

#[test]
fn a_blank_row_takes_no_properties() {
    let mut ed = editor();
    ed.append_row(|ed, uid| {
        let err = ed.set_priority(uid, 10).unwrap_err();
        assert!(err.contains("blank"));
        Ok(())
    })
    .unwrap();
}
