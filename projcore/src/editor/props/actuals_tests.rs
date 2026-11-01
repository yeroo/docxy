use super::actuals::ELAPSED_NO_WORKING_TIME;
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

// ---- elapsed tasks (#484) ----

/// An elapsed (`DurationFormat` 8) task `uid` of 72 elapsed hours, started
/// Mon 2 March 08:00 with `actual` minutes done, in a plan starting then.
fn started_elapsed(uid: i32, actual: Option<i64>) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("E{uid}"),
        outline_level: 1,
        duration_min: 4320,
        duration_format: Some(8),
        actual_start: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
        actual_duration_min: actual,
        ..Task::default()
    }
}

fn elapsed_plan(tasks: Vec<Task>) -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0)),
        tasks,
        ..Project::default()
    })
}

/// Progress on an elapsed task is re-expressed over its wall-clock window
/// (72 elapsed hours from Mon 08:00 cover three working days, to Thu 08:00),
/// not its scheduled span, which the tracked path already inflates, and the
/// duration is working time from then on (r1 M1, r2 M1).
#[test]
fn progress_on_an_elapsed_task_counts_its_wall_clock_window() {
    let mut unstarted = started_elapsed(2, None);
    unstarted.actual_start = None;
    let mut ed = elapsed_plan(vec![started_elapsed(1, Some(1440)), unstarted]);
    ed.set_percent_complete_synced(1, 50).unwrap();
    let t = ed.project().task(1).unwrap();
    assert_eq!((t.duration_min, t.duration_format), (3 * 480, None));
    assert_eq!(
        (
            t.actual_duration_min,
            t.remaining_duration_min,
            t.percent_complete
        ),
        (Some(720), Some(720), Some(50))
    );
    // Unstarted, from its scheduled start: an actual of one day is a third.
    ed.set_actual_duration(2, 480).unwrap();
    let t = ed.project().task(2).unwrap();
    assert_eq!((t.duration_min, t.duration_format), (3 * 480, None));
    assert_eq!(
        (
            t.actual_duration_min,
            t.remaining_duration_min,
            t.percent_complete
        ),
        (Some(480), Some(960), Some(33))
    );
}

/// A remaining duration whose new working duration equals the stored
/// elapsed minutes is still a duration change: one undo step of its own
/// (r2 m2), committing an estimate as any other value does (r3 i2).
#[test]
fn a_remaining_edit_to_the_stored_elapsed_minutes_is_a_duration_change() {
    let estimated = |uid| Task {
        duration_format: Some(40),
        estimated: Some(true),
        ..started_elapsed(uid, None)
    };
    let plain = Task {
        uid: 3,
        id: 3,
        name: "Plain".into(),
        outline_level: 1,
        duration_min: 480,
        ..Task::default()
    };
    let mut ed = elapsed_plan(vec![estimated(1), estimated(2), plain]);
    ed.rename(3, "Renamed").unwrap();
    let before = ed.project().task(1).unwrap().clone();
    let depth = ed.undo_depth();
    for (uid, minutes) in [(1, 4320), (2, 4380)] {
        assert_eq!(ed.set_remaining_duration(uid, minutes), Ok(true));
        let t = ed.project().task(uid).unwrap();
        assert_eq!(
            (
                t.duration_min,
                t.duration_format,
                t.estimated,
                t.remaining_duration_min
            ),
            (minutes, None, Some(false), Some(minutes)),
            "{minutes}"
        );
    }
    assert_eq!(ed.undo_depth(), depth + 2);
    assert!(ed.undo());
    assert!(ed.undo());
    assert_eq!(ed.project().task(1).unwrap(), &before);
    assert_eq!(ed.project().task(3).unwrap().name, "Renamed");
}

/// An elapsed task whose wall-clock window holds no working time (1 eday
/// from Sat 00:00 by a Start No Earlier Than; 2 edays started Fri 17:00)
/// takes no progress edit: its working duration would be 0, a milestone
/// (r3 M2). The task and the undo stack are unchanged.
#[test]
fn progress_on_an_elapsed_task_spanning_no_working_time_is_refused() {
    let sat = DateTime::from_ymd_hm(2026, 3, 7, 0, 0);
    let fri = DateTime::from_ymd_hm(2026, 3, 6, 17, 0);
    let weekend = Task {
        duration_min: 1440,
        actual_start: None,
        constraint: ConstraintType::StartNoEarlierThan,
        constraint_date: Some(sat),
        ..started_elapsed(1, None)
    };
    let friday = Task {
        duration_min: 2880,
        actual_start: Some(fri),
        ..started_elapsed(2, None)
    };
    let mut ed = elapsed_plan(vec![weekend, friday]);
    let refused = Err(ELAPSED_NO_WORKING_TIME.to_string());
    for uid in [1, 2] {
        let before = ed.project().task(uid).unwrap().clone();
        assert_eq!(ed.set_actual_duration(uid, 480), refused, "{uid}");
        assert_eq!(ed.set_remaining_duration(uid, 480), refused, "{uid}");
        assert_eq!(ed.set_actual_finish(uid, Some(sat)), refused, "{uid}");
        assert_eq!(ed.set_actual_start(uid, Some(fri)), refused, "{uid}");
        assert_eq!(ed.project().task(uid).unwrap(), &before, "{uid}");
    }
    assert_eq!(ed.set_percent_complete_synced(2, 50), refused);
    assert_eq!(ed.undo_depth(), 0);
}

/// Task 1 elapsed (72 elapsed hours from Mon 08:00, unstarted), typed `kind`
/// and effort-driven per `driven`, with Alice at 100% for 4320 minutes of
/// work counted over those elapsed minutes; Bob is free.
fn elapsed_with_work(kind: Option<TaskType>, driven: Option<bool>) -> Editor {
    let person = |uid: i32, name: &str| Resource {
        uid,
        id: uid,
        name: name.into(),
        kind: ResourceType::Work,
        max_units: 1.0,
        ..Resource::default()
    };
    let mut ed = elapsed_plan(vec![Task {
        actual_start: None,
        task_type: kind,
        effort_driven: driven,
        ..started_elapsed(1, None)
    }]);
    ed.proj.resources = vec![person(1, "Alice"), person(2, "Bob")];
    ed.proj.assignments = vec![Assignment {
        uid: 1,
        task_uid: 1,
        resource_uid: 1,
        units: 1.0,
        work_min: 4320,
        ..Assignment::default()
    }];
    ed
}

/// `(units, work, remaining work)` of resource `rid`'s assignment on task 1.
fn alloc_of(ed: &Editor, rid: i32) -> (f64, i64, Option<i64>) {
    let a = ed
        .project()
        .assignments
        .iter()
        .find(|a| a.task_uid == 1 && a.resource_uid == rid)
        .unwrap();
    (a.units, a.work_min, a.remaining_work_min)
}

/// r4 M1: a progress edit that moves an elapsed task to working time takes
/// its assignments' work along at the same units (Fixed Work included: the
/// span is only re-expressed), in the edit's one undo step, and the refresh
/// derives the remaining work from it.
#[test]
fn progress_on_an_elapsed_task_rescales_its_work_to_working_time() {
    for kind in [
        None,
        Some(TaskType::FixedDuration),
        Some(TaskType::FixedWork),
    ] {
        let mut ed = elapsed_with_work(kind, None);
        let before = ed.project().clone();
        assert_eq!(ed.set_actual_duration(1, 480), Ok(true), "{kind:?}");
        assert_eq!(ed.project().task(1).unwrap().duration_min, 1440, "{kind:?}");
        assert_eq!(alloc_of(&ed, 1), (1.0, 1440, Some(1440)), "{kind:?}");
        assert_eq!(ed.undo_depth(), 1);
        assert!(ed.undo());
        assert_eq!(ed.project().assignments, before.assignments, "{kind:?}");
        assert_eq!(ed.project().tasks, before.tasks, "{kind:?}");
    }
}

/// r4 M1: after the progress edit an effort-driven task's work is working
/// work, so a resource added later shares 1440 minutes, not 4320.
#[test]
fn an_effort_driven_elapsed_task_shares_its_rescaled_work() {
    let mut ed = elapsed_with_work(None, Some(true));
    ed.set_actual_duration(1, 480).unwrap();
    ed.set_resources(1, &["Alice".to_string(), "Bob".to_string()])
        .unwrap();
    assert_eq!(ed.project().task(1).unwrap().duration_min, 720);
    assert_eq!((alloc_of(&ed, 1).1, alloc_of(&ed, 2).1), (720, 720));
}

/// r4 M1: a remaining-duration edit on an elapsed task first re-expresses
/// its work over the same span in working time (4320 elapsed minutes at
/// 100% become 1440 working), then rescales it by the duration change as a
/// typed duration does: Fixed Units work follows the 2000 minutes, Fixed
/// Work keeps 1440 and lowers its units, never 4320 over 2000.
#[test]
fn a_remaining_edit_on_an_elapsed_task_converts_the_work_before_rescaling_it() {
    for (kind, want) in [
        (None, (1.0, 2000)),
        (Some(TaskType::FixedWork), (0.72, 1440)),
    ] {
        let mut ed = elapsed_with_work(kind, None);
        assert_eq!(ed.set_remaining_duration(1, 2000), Ok(true), "{kind:?}");
        let t = ed.project().task(1).unwrap();
        assert_eq!(
            (t.duration_min, t.duration_format),
            (2000, None),
            "{kind:?}"
        );
        let (units, work, _) = alloc_of(&ed, 1);
        assert_eq!(((units * 100.0).round() / 100.0, work), want, "{kind:?}");
        assert_eq!(ed.undo_depth(), 1);
    }
}
