use super::*;
use crate::mspdi::{read_mspdi, write_mspdi};

/// A summary (1), a 32 h task `Pour` (2, four 8 h days) and a milestone (3).
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
                name: "Pour".into(),
                outline_level: 2,
                duration_min: 1920,
                ..Task::default()
            },
            Task {
                uid: 3,
                id: 3,
                name: "Done".into(),
                outline_level: 2,
                duration_min: 0,
                milestone: true,
                ..Task::default()
            },
        ],
        ..Project::default()
    })
}

fn round_trip(ed: &Editor) -> Project {
    read_mspdi(&write_mspdi(ed.project())).unwrap()
}

fn pour(ed: &Editor) -> &Task {
    ed.project().task(2).unwrap()
}

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 1, day, hour, 0)
}

#[test]
fn actual_duration_sets_percent_remaining_and_keeps_the_duration() {
    let mut ed = editor();
    assert_eq!(ed.set_actual_duration(2, 960), Ok(true));
    let t = pour(&ed);
    assert_eq!(t.percent_complete, Some(50));
    assert_eq!(t.actual_duration_min, Some(960));
    assert_eq!(t.remaining_duration_min, Some(960));
    assert_eq!(t.duration_min, 1920);
    assert_eq!(t.actual_start, Some(at(5, 8)), "work recorded starts it");
    assert_eq!(ed.set_actual_duration(2, 960), Ok(false));
    assert_eq!(ed.undo_depth(), 1);
    let back = round_trip(&ed);
    let t = back.task(2).unwrap();
    assert_eq!(
        (
            t.percent_complete,
            t.actual_duration_min,
            t.remaining_duration_min,
            t.actual_start
        ),
        (Some(50), Some(960), Some(960), Some(at(5, 8)))
    );
    assert!(ed.undo());
    let t = pour(&ed);
    assert_eq!(
        (t.actual_start, t.actual_duration_min, t.percent_complete),
        (None, None, None)
    );
}

#[test]
fn actual_duration_bounds_and_the_whole_duration() {
    let mut ed = editor();
    assert!(
        ed.set_actual_duration(2, 1921)
            .unwrap_err()
            .contains("exceeds")
    );
    assert!(ed.set_actual_duration(2, -1).is_err());
    assert_eq!(ed.undo_depth(), 0);
    assert_eq!(ed.set_actual_duration(2, 1920), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (t.percent_complete, t.remaining_duration_min),
        (Some(100), Some(0))
    );
    assert_eq!(t.actual_finish, None, "no finish is invented");
}

#[test]
fn the_schedule_follows_the_remaining_duration() {
    let mut ed = editor();
    ed.set_actual_duration(2, 960).unwrap();
    let before = ed.schedule().get(2).unwrap().early_finish;
    assert_eq!(ed.set_remaining_duration(2, 480), Ok(true));
    let t = pour(&ed);
    assert_eq!(t.duration_min, 1440);
    assert_eq!(t.percent_complete, Some(67));
    assert_eq!(t.actual_duration_min, Some(960));
    assert_eq!(t.remaining_duration_min, Some(480));
    assert!(ed.schedule().get(2).unwrap().early_finish < before);
    assert_eq!(round_trip(&ed).task(2).unwrap().duration_min, 1440);
    assert_eq!(ed.undo_depth(), 2, "one step per setter");
    assert!(ed.undo());
    assert_eq!(pour(&ed).duration_min, 1920);
    assert_eq!(pour(&ed).remaining_duration_min, Some(960));
    assert_eq!(ed.set_remaining_duration(2, 960), Ok(false));
}

#[test]
fn remaining_duration_uses_the_percent_when_no_actual_is_recorded() {
    let mut ed = editor();
    ed.set_percent_complete(2, 50).unwrap();
    assert_eq!(ed.set_remaining_duration(2, 480), Ok(true));
    let t = pour(&ed);
    assert_eq!((t.duration_min, t.percent_complete), (1440, Some(67)));
    assert_eq!(t.actual_start, None, "it does not start the task");
}

#[test]
fn remaining_zero_completes_unless_nothing_is_done() {
    let mut ed = editor();
    let err = ed.set_remaining_duration(2, 0).unwrap_err();
    assert!(err.contains("zero duration"), "{err}");
    assert_eq!(ed.undo_depth(), 0);
    ed.set_actual_duration(2, 480).unwrap();
    assert_eq!(ed.set_remaining_duration(2, 0), Ok(true));
    let t = pour(&ed);
    assert_eq!((t.duration_min, t.percent_complete), (480, Some(100)));
    assert_eq!(t.actual_finish, None);
}

#[test]
fn actual_finish_completes_and_clearing_the_start_resets() {
    let mut ed = editor();
    assert_eq!(ed.set_actual_finish(2, Some(at(8, 17))), Ok(true));
    let t = pour(&ed);
    assert_eq!(t.actual_finish, Some(at(8, 17)));
    assert_eq!(t.actual_start, Some(at(5, 8)), "the scheduled start");
    assert_eq!(
        (
            t.percent_complete,
            t.actual_duration_min,
            t.remaining_duration_min
        ),
        (Some(100), Some(1920), None)
    );
    assert_eq!(
        round_trip(&ed).task(2).unwrap().actual_finish,
        Some(at(8, 17))
    );
    assert_eq!(ed.set_actual_start(2, None), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (
            t.actual_start,
            t.actual_finish,
            t.actual_duration_min,
            t.percent_complete
        ),
        (None, None, None, Some(0))
    );
    assert!(ed.undo());
    assert_eq!(pour(&ed).actual_finish, Some(at(8, 17)));
}

#[test]
fn clearing_the_actual_finish_keeps_the_share() {
    let mut ed = editor();
    assert_eq!(ed.set_actual_finish(2, None), Ok(false));
    ed.set_actual_finish(2, Some(at(8, 17))).unwrap();
    ed.set_actual_duration(2, 1920).unwrap();
    assert_eq!(ed.set_actual_finish(2, None), Ok(true));
    let t = pour(&ed);
    assert_eq!((t.actual_finish, t.percent_complete), (None, Some(100)));
    assert_eq!(t.actual_start, Some(at(5, 8)));
}

#[test]
fn dates_are_checked_against_each_other() {
    let mut ed = editor();
    ed.set_actual_start(2, Some(at(6, 8))).unwrap();
    assert!(ed.set_actual_finish(2, Some(at(5, 17))).is_err());
    ed.set_actual_finish(2, Some(at(7, 17))).unwrap();
    assert!(ed.set_actual_start(2, Some(at(8, 8))).is_err());
    assert!(
        ed.set_actual_duration(2, 480)
            .unwrap_err()
            .contains("actual finish")
    );
    assert!(ed.set_remaining_duration(2, 480).is_err());
    assert_eq!(ed.undo_depth(), 2);
}

#[test]
fn a_stale_stop_is_dropped_with_the_actual() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.actual_start = Some(at(5, 8));
    t.actual_duration_min = Some(480);
    t.stop = Some(at(5, 17));
    t.resume = Some(at(6, 8));
    let mut ed = Editor::new(proj);
    ed.set_actual_duration(2, 960).unwrap();
    assert_eq!((pour(&ed).stop, pour(&ed).resume), (None, None));
}

#[test]
fn summaries_blank_rows_and_milestones_are_refused() {
    let mut ed = editor();
    for uid in [1, 3] {
        assert!(ed.set_actual_start(uid, Some(at(5, 8))).is_err());
        assert!(ed.set_actual_finish(uid, Some(at(5, 8))).is_err());
        assert!(ed.set_actual_duration(uid, 0).is_err());
        assert!(ed.set_remaining_duration(uid, 60).is_err());
    }
    assert!(ed.set_actual_start(9, None).is_err());
    assert_eq!(ed.undo_depth(), 0);
}
