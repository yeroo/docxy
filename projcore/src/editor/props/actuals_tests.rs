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
                duration_min: 1920,
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
            Task {
                uid: 4,
                id: 4,
                is_null: true,
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
    let err = ed.set_actual_duration(1, 480).unwrap_err();
    assert!(err.contains("summary"), "the summary guard refuses: {err}");
    for uid in [1, 3, 4] {
        assert!(ed.set_actual_start(uid, Some(at(5, 8))).is_err());
        assert!(ed.set_actual_finish(uid, Some(at(5, 8))).is_err());
        assert!(ed.set_actual_duration(uid, 0).is_err());
        assert!(ed.set_remaining_duration(uid, 60).is_err());
    }
    assert!(ed.set_actual_start(9, None).is_err());
    assert_eq!(ed.undo_depth(), 0);
}

#[test]
fn a_remaining_duration_keeps_the_stop_and_resume() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.actual_start = Some(at(5, 8));
    t.actual_duration_min = Some(960);
    t.stop = Some(at(6, 17));
    t.resume = Some(at(8, 8));
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_remaining_duration(2, 480), Ok(true));
    assert_eq!(
        (pour(&ed).stop, pour(&ed).resume),
        (Some(at(6, 17)), Some(at(8, 8)))
    );
    assert_eq!(ed.set_actual_duration(2, 480), Ok(true));
    assert_eq!(pour(&ed).stop, None, "the completed part moved");
}

#[test]
fn the_whole_duration_on_a_finished_task_is_a_no_op() {
    let mut ed = editor();
    ed.set_actual_finish(2, Some(at(8, 17))).unwrap();
    let depth = ed.undo_depth();
    assert_eq!(ed.set_actual_duration(2, 1920), Ok(false));
    assert_eq!(ed.set_remaining_duration(2, 0), Ok(false));
    assert_eq!(ed.undo_depth(), depth);
    assert_eq!(pour(&ed).remaining_duration_min, None);
}

#[test]
fn a_manual_task_restamps_its_saved_dates() {
    let mut ed = editor();
    ed.set_manual(2, true).unwrap();
    ed.set_actual_finish(2, Some(at(12, 17))).unwrap();
    assert_eq!(pour(&ed).stored_finish, Some(at(12, 17)));
    assert_eq!(
        round_trip(&ed).task(2).unwrap().stored_finish,
        Some(at(12, 17))
    );
}

#[test]
fn a_remaining_duration_too_long_is_refused() {
    let mut ed = editor();
    ed.set_actual_duration(2, 480).unwrap();
    let depth = ed.undo_depth();
    let err = ed.set_remaining_duration(2, i64::MAX).unwrap_err();
    assert!(err.contains("too long"), "{err}");
    assert_eq!(ed.undo_depth(), depth);
}

#[test]
fn a_started_task_asked_for_its_percent_is_unchanged() {
    let mut ed = editor();
    ed.set_actual_start(2, Some(at(5, 8))).unwrap();
    let depth = ed.undo_depth();
    assert_eq!(ed.set_percent_complete(2, 0), Ok(false));
    assert_eq!(ed.undo_depth(), depth);
    ed.set_percent_complete(2, 30).unwrap();
    assert!(
        ed.set_percent_complete(2, 0)
            .unwrap_err()
            .contains("actual start")
    );
}

#[test]
fn starting_a_task_pins_down_its_done_share() {
    let with = |percent, remaining| {
        let mut proj = editor().project().clone();
        let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
        t.percent_complete = percent;
        t.remaining_duration_min = remaining;
        Editor::new(proj)
    };
    // A stale stored remaining duration (the duration was edited since).
    let mut ed = with(None, Some(480));
    assert_eq!(ed.set_actual_start(2, Some(at(5, 8))), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (t.actual_duration_min, t.remaining_duration_min),
        (Some(0), Some(1920))
    );
    let Some(crate::model::Tracked::InProgress { remaining_min, .. }) = t.tracked() else {
        panic!("in progress")
    };
    assert_eq!(remaining_min, 1920);
    // A % Complete the actual start now makes real.
    let mut ed = with(Some(50), Some(1920));
    ed.set_actual_start(2, Some(at(5, 8))).unwrap();
    let t = pour(&ed);
    assert_eq!(
        (t.actual_duration_min, t.remaining_duration_min),
        (Some(960), Some(960))
    );
}

#[test]
fn the_stop_check_compares_with_the_task_before_the_duration_edit() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.actual_start = Some(at(5, 8));
    t.percent_complete = Some(50);
    t.stop = Some(at(6, 17));
    t.resume = Some(at(8, 8));
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_remaining_duration(2, 480), Ok(true));
    assert_eq!(
        pour(&ed).resume,
        Some(at(8, 8)),
        "the work done is the same 960"
    );
}

#[test]
fn clearing_the_finish_of_an_import_with_no_actual_duration_pins_the_share() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.actual_start = Some(at(5, 8));
    t.actual_finish = Some(at(8, 17));
    t.percent_complete = Some(100);
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_actual_finish(2, None), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (t.percent_complete, t.actual_duration_min),
        (Some(99), Some(1901))
    );
    let Some(crate::model::Tracked::InProgress { remaining_min, .. }) = t.tracked() else {
        panic!("in progress")
    };
    assert_eq!(remaining_min, 1920 - 1901);
}

#[test]
fn a_synced_percent_leaves_a_matching_started_task_alone() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.actual_start = Some(at(5, 8));
    t.actual_duration_min = Some(1000);
    t.remaining_duration_min = Some(920);
    t.percent_complete = Some(52);
    t.stop = Some(at(6, 17));
    t.resume = Some(at(8, 8));
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_percent_complete_synced(2, 52), Ok(false));
    assert_eq!(ed.undo_depth(), 0);
    let t = pour(&ed);
    assert_eq!(
        (t.actual_duration_min, t.resume),
        (Some(1000), Some(at(8, 8)))
    );
    // Another percent moves the actual with it, in one step.
    assert_eq!(ed.set_percent_complete_synced(2, 50), Ok(true));
    assert_eq!(ed.undo_depth(), 1);
    let t = pour(&ed);
    assert_eq!(
        (t.actual_duration_min, t.remaining_duration_min),
        (Some(960), Some(960))
    );
    assert_eq!(t.resume, None);
    assert!(ed.undo());
    assert_eq!(pour(&ed).percent_complete, Some(52));
}

#[test]
fn a_synced_percent_records_the_actual_of_a_started_task_only() {
    let mut ed = editor();
    assert_eq!(ed.set_percent_complete_synced(2, 30), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (t.percent_complete, t.actual_duration_min, t.actual_start),
        (Some(30), None, None)
    );
    ed.set_actual_start(2, Some(at(5, 8))).unwrap();
    assert_eq!(ed.set_percent_complete_synced(2, 100), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (
            t.actual_duration_min,
            t.remaining_duration_min,
            t.actual_finish
        ),
        (Some(1920), Some(0), None)
    );
    assert!(ed.set_percent_complete_synced(2, 101).is_err());
    assert!(
        ed.set_percent_complete_synced(2, 0).is_err(),
        "started, not 0%"
    );
}

#[test]
fn an_external_task_takes_no_progress() {
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.external_task = Some(true);
    t.actual_start = Some(at(5, 8));
    let mut ed = Editor::new(proj);
    assert_eq!(
        ed.set_actual_start(2, Some(at(6, 8))).unwrap_err(),
        EXTERNAL_TASK_DATES
    );
    assert!(ed.set_actual_finish(2, Some(at(7, 17))).is_err());
    assert!(ed.set_actual_duration(2, 480).is_err());
    assert!(ed.set_remaining_duration(2, 480).is_err());
    assert!(ed.set_percent_complete_synced(2, 40).is_err());
    assert_eq!(ed.undo_depth(), 0);
}

#[test]
fn a_synced_percent_follows_a_duration_that_moved() {
    // 4 d task at 50 % with 2 d done, whose duration is then made 6 d.
    let mut proj = editor().project().clone();
    let t = proj.tasks.iter_mut().find(|t| t.uid == 2).unwrap();
    t.duration_min = 1920 * 3 / 2;
    t.actual_start = Some(at(5, 8));
    t.actual_duration_min = Some(960);
    t.remaining_duration_min = Some(960);
    t.percent_complete = Some(33);
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_percent_complete_synced(2, 50), Ok(true));
    let t = pour(&ed);
    assert_eq!(
        (t.actual_duration_min, t.percent_complete),
        (Some(1440), Some(50))
    );
    // The same 50 % on a task whose actual is stale for it (percent already 50).
    let mut proj = ed.project().clone();
    proj.tasks
        .iter_mut()
        .find(|t| t.uid == 2)
        .unwrap()
        .actual_duration_min = Some(960);
    let mut ed = Editor::new(proj);
    assert_eq!(ed.set_percent_complete_synced(2, 50), Ok(true));
    assert_eq!(pour(&ed).actual_duration_min, Some(1440));
}
