use super::*;
use crate::model::{Calendar, DayWorking, WorkingTime};

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 3, day, hour, 0)
}

/// Task 1 of three days from Mon 2 March 08:00, typed `kind`, with Alice at
/// 100% on it, and Bob at 100% for a third of its work after a two-hour
/// Delay (working time on every task, r8). `elapsed`: 72 elapsed hours (format 8) and Alice's work
/// counted over them; else the same task converted by hand, three working
/// days (no format), every minute a third. `started`: a third done (actual
/// duration and Alice's actual work), from Mon 08:00.
fn plan(elapsed: bool, kind: Option<TaskType>, manual: bool, started: bool) -> Editor {
    let m = |working: i64| if elapsed { 3 * working } else { working };
    Editor::new(Project {
        start_date: Some(at(2, 8)),
        tasks: vec![Task {
            uid: 1,
            id: 1,
            name: "E".into(),
            outline_level: 1,
            duration_min: m(1440),
            duration_format: elapsed.then_some(8),
            task_type: kind,
            manual,
            manual_start: manual.then(|| at(2, 8)),
            manual_duration_min: manual.then(|| m(1440)),
            actual_start: started.then(|| at(2, 8)),
            actual_duration_min: started.then(|| m(480)),
            remaining_duration_min: started.then(|| m(960)),
            percent_complete: started.then_some(33),
            ..Task::default()
        }],
        resources: vec![
            Resource {
                uid: 1,
                id: 1,
                name: "Alice".into(),
                kind: ResourceType::Work,
                max_units: 1.0,
                ..Resource::default()
            },
            Resource {
                uid: 2,
                id: 2,
                name: "Bob".into(),
                kind: ResourceType::Work,
                max_units: 1.0,
                ..Resource::default()
            },
        ],
        assignments: vec![
            Assignment {
                uid: 1,
                task_uid: 1,
                resource_uid: 1,
                units: 1.0,
                work_min: m(1440),
                actual_work_min: started.then(|| m(480)),
                ..Assignment::default()
            },
            Assignment {
                uid: 2,
                task_uid: 1,
                resource_uid: 2,
                units: 1.0,
                work_min: m(480),
                delay: Some(120 * 10),
                ..Assignment::default()
            },
        ],
        ..Project::default()
    })
}

/// Every entry point that writes working minutes over a task.
#[derive(Clone, Copy, Debug)]
enum Entry {
    DurationMin,
    /// r6 M4: the stored elapsed minutes' number, as working minutes.
    DurationMinOfElapsed,
    /// r6 M4: `9d?`, the same number, estimated.
    TypedEstimatedOfElapsed,
    /// r6 M4: Ctrl+Delete to the same number.
    ResetOfElapsed,
    /// r6 M3: the minutes the conversion gives (`3d` on three working days).
    TypedConverted,
    UpdateTask,
    ManualFinish,
    ActualStart,
    ActualFinish,
    ActualDuration,
    PercentSynced,
    RemainingDuration,
}

impl Entry {
    fn apply(self, ed: &mut Editor) -> Result<(), String> {
        match self {
            Entry::DurationMin => ed.set_duration_min(1, 2000, false),
            Entry::DurationMinOfElapsed => ed.set_duration_min(1, 4320, false),
            Entry::TypedEstimatedOfElapsed => ed.set_duration(1, "9d?"),
            Entry::ResetOfElapsed => ed.reset_duration(1, 4320, false),
            Entry::TypedConverted => ed.set_duration(1, "3d"),
            Entry::UpdateTask => ed.update_task(
                1,
                TaskPatch {
                    duration_min: Some(2000),
                    ..TaskPatch::default()
                },
            ),
            Entry::ManualFinish => ed.set_finish_at(1, at(5, 17)),
            Entry::ActualStart => ed.set_actual_start(1, Some(at(3, 8))).map(drop),
            Entry::ActualFinish => ed.set_actual_finish(1, Some(at(4, 17))).map(drop),
            Entry::ActualDuration => ed.set_actual_duration(1, 600).map(drop),
            Entry::PercentSynced => ed.set_percent_complete_synced(1, 60).map(drop),
            Entry::RemainingDuration => ed.set_remaining_duration(1, 1000).map(drop),
        }
    }
}

/// What a write decides on task 1, Alice's assignment and Bob's delayed one
/// (with his span) (the refresh's own
/// derived fields left out: it runs against different "before" states).
fn facts(ed: &Editor) -> String {
    let t = ed.project().task(1).unwrap();
    let a = &ed.project().assignments[0];
    let bob = ed.project().assignments.get(1).map(|b| {
        let span = crate::assign::assignment_span(ed.project(), ed.schedule(), b);
        format!("{:.6} {} {:?} {:?}", b.units, b.work_min, b.delay, span)
    });
    format!(
        "duration {} format {:?} estimated {:?} manual {:?} pin {:?}..{:?} \
         actual {:?}..{:?} {:?} remaining {:?} percent {:?} constraint {:?} \
         | units {:.6} work {} actual work {:?} overtime {:?} | bob {bob:?}",
        t.duration_min,
        t.duration_format,
        t.estimated,
        t.manual_duration_min,
        t.manual_start,
        t.manual_finish,
        t.actual_start,
        t.actual_finish,
        t.actual_duration_min,
        t.remaining_duration_min,
        t.percent_complete,
        t.constraint,
        a.units,
        a.work_min,
        a.actual_work_min,
        a.actual_overtime_work_min,
    )
}

/// r5, r6: every write of working minutes over an elapsed task gives what the
/// same write gives on that task converted to working time by hand, for
/// every task type, manual or not, unstarted or in progress with actual
/// work (r5 M1: a Fixed Work task's units, M2: the actual work), as one
/// undo step that restores everything; a refused write changes nothing.
#[test]
fn a_working_write_on_an_elapsed_task_equals_it_on_the_working_task() {
    let writes = [
        Entry::DurationMin,
        Entry::DurationMinOfElapsed,
        Entry::TypedEstimatedOfElapsed,
        Entry::ResetOfElapsed,
        Entry::TypedConverted,
        Entry::UpdateTask,
        Entry::ManualFinish,
        Entry::ActualStart,
        Entry::ActualFinish,
        Entry::ActualDuration,
        Entry::PercentSynced,
        Entry::RemainingDuration,
    ];
    let kinds = [
        None,
        Some(TaskType::FixedDuration),
        Some(TaskType::FixedWork),
    ];
    let mut compared = 0;
    for write in writes {
        for kind in kinds {
            for manual in [false, true] {
                for started in [false, true] {
                    // An auto task's finish is a constraint, and an
                    // unstarted task's % Complete writes no minutes.
                    if matches!(write, Entry::ManualFinish) && !manual
                        || matches!(write, Entry::PercentSynced) && !started
                    {
                        continue;
                    }
                    let case = format!("{write:?} {kind:?} manual {manual} started {started}");
                    let mut working = plan(false, kind, manual, started);
                    let mut elapsed = plan(true, kind, manual, started);
                    let before = elapsed.project().clone();
                    let want = write.apply(&mut working);
                    let got = write.apply(&mut elapsed);
                    assert_eq!(got, want, "{case}");
                    if got.is_err() {
                        assert_eq!(elapsed.project(), &before, "{case}");
                        assert_eq!(elapsed.undo_depth(), 0, "{case}");
                        continue;
                    }
                    assert_eq!(facts(&elapsed), facts(&working), "{case}");
                    assert_eq!(elapsed.undo_depth(), 1, "{case}");
                    assert!(elapsed.undo(), "{case}");
                    assert_eq!(elapsed.project(), &before, "{case}");
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 3 * (2 * 2 * 10 + 2 + 2), "every case wrote");
}

/// The conversion alone: duration, actual and remaining durations, the pin,
/// the assignment's work and actual work by the window's factor (72 elapsed
/// hours from Mon 08:00 hold 24 working hours), units kept, the format to
/// days; and it changes nothing when the edit after it does nothing.
#[test]
fn the_conversion_scales_the_whole_task_by_its_window() {
    let mut ed = plan(true, Some(TaskType::FixedWork), true, true);
    ed.elapsed_to_working(0, Write::Progress).unwrap();
    let want = plan(false, Some(TaskType::FixedWork), true, true);
    assert_eq!(facts(&ed), facts(&want));
    // An edit that changes nothing rolls its conversion back.
    let mut ed = plan(true, None, false, true);
    let before = ed.project().clone();
    let i = ed.index(1).unwrap();
    assert_eq!(
        ed.in_working_time(i, Write::Progress, |_| Ok(false)),
        Ok(false)
    );
    assert_eq!(ed.project(), &before);
    assert_eq!(ed.undo_depth(), 0);
}

/// r6 M1: a write that changes nothing on the working task changes nothing
/// on the elapsed one either, also inside an outer batch (projctl
/// `task.set`, a plugin's ops, a paste, Task Information's OK): no
/// conversion, no undo step, no dirty mark.
#[test]
fn a_no_op_write_inside_a_batch_leaves_an_elapsed_task_elapsed() {
    type Echo = fn(&mut Editor) -> Result<(), String>;
    let echoes: [(&str, Echo); 7] = [
        ("no actual start", |ed| {
            ed.set_actual_start(1, None).map(drop)
        }),
        ("no actual finish", |ed| {
            ed.set_actual_finish(1, None).map(drop)
        }),
        ("same actual start", |ed| {
            ed.set_actual_start(1, Some(at(2, 8))).map(drop)
        }),
        ("same actual duration", |ed| {
            ed.set_actual_duration(1, 480).map(drop)
        }),
        ("same remaining", |ed| {
            ed.set_remaining_duration(1, 960).map(drop)
        }),
        ("same percent", |ed| {
            ed.set_percent_complete_synced(1, 33).map(drop)
        }),
        ("its own pinned finish", |ed| ed.set_finish_at(1, at(5, 8))),
    ];
    let mut seen = 0;
    for (name, echo) in echoes {
        for kind in [None, Some(TaskType::FixedWork)] {
            for started in [false, true] {
                let case = format!("{name} {kind:?} started {started}");
                let mut working = plan(false, kind, true, started);
                pin_finish(&mut working, at(5, 8));
                let mut elapsed = plan(true, kind, true, started);
                pin_finish(&mut elapsed, at(5, 8));
                let before = working.project().clone();
                let ok = working.batch(echo).is_ok();
                if !ok || working.project() != &before || working.undo_depth() != 0 {
                    continue;
                }
                let before = elapsed.project().clone();
                assert_eq!(elapsed.batch(echo), Ok(()), "{case}");
                assert_eq!(elapsed.project(), &before, "{case}");
                assert_eq!(elapsed.undo_depth(), 0, "{case}");
                assert!(!elapsed.dirty(), "{case}");
                // Nor in a batch that goes on to change something else.
                let renamed = elapsed.batch(|ed| {
                    echo(ed)?;
                    ed.rename(1, "F")
                });
                assert_eq!(renamed, Ok(()), "{case}");
                let t = elapsed.project().task(1).unwrap();
                assert_eq!(t.duration_format, Some(8), "{case}");
                assert_eq!(t.duration_min, 4320, "{case}");
                seen += 1;
            }
        }
    }
    assert!(
        seen >= 14,
        "the echoes are no-ops on the working task: {seen}"
    );
}

/// A manual task's pinned finish (`plan` pins none).
fn pin_finish(ed: &mut Editor, finish: DateTime) {
    let mut proj = ed.project().clone();
    proj.tasks[0].manual_finish = Some(finish);
    *ed = Editor::new(proj);
}

/// r6 M1: a refused write inside a batch whose caller goes on leaves no
/// conversion behind either.
#[test]
fn a_refused_write_inside_a_batch_leaves_an_elapsed_task_elapsed() {
    let mut ed = plan(true, None, false, true);
    let before = ed.project().clone();
    let result = ed.batch(|ed| {
        // An actual finish before the actual start is refused; the caller
        // carries on.
        let _ = ed.set_actual_finish(1, Some(at(1, 8)));
        Ok(())
    });
    assert_eq!(result, Ok(()));
    assert_eq!(ed.project(), &before);
    assert_eq!(ed.undo_depth(), 0);
}

/// One leaf of `minutes` in `format` from the project start `start`,
/// typed `kind`, manual (pinned there) or not, with Alice at 100% and her
/// work over those minutes, unstarted.
fn window_plan(
    start: DateTime,
    minutes: i64,
    format: Option<u8>,
    kind: Option<TaskType>,
    manual: bool,
) -> Editor {
    Editor::new(Project {
        start_date: Some(start),
        tasks: vec![Task {
            uid: 1,
            id: 1,
            name: "E".into(),
            outline_level: 1,
            duration_min: minutes,
            duration_format: format,
            task_type: kind,
            manual,
            manual_start: manual.then_some(start),
            manual_duration_min: manual.then_some(minutes),
            ..Task::default()
        }],
        resources: vec![Resource {
            uid: 1,
            id: 1,
            name: "Alice".into(),
            kind: ResourceType::Work,
            max_units: 1.0,
            ..Resource::default()
        }],
        assignments: vec![Assignment {
            uid: 1,
            task_uid: 1,
            resource_uid: 1,
            units: 1.0,
            work_min: minutes,
            ..Assignment::default()
        }],
        ..Project::default()
    })
}

/// r6 M2: an elapsed task whose window holds no working time (12 elapsed
/// hours from 17:00, two elapsed days over a weekend) takes a write that
/// replaces its duration: zero working minutes first, its work by the rule
/// of a duration going to zero (a Fixed Work task keeps it), then the
/// write, as on that working task.
#[test]
fn a_replacing_write_on_an_elapsed_task_with_no_working_time_applies() {
    type Replace = fn(&mut Editor) -> Result<(), String>;
    let writes: [(&str, Replace); 4] = [
        ("typed", |ed| ed.set_duration(1, "2d")),
        ("minutes", |ed| ed.set_duration_min(1, 600, true)),
        ("reset", |ed| ed.reset_duration(1, 480, true)),
        ("manual finish", |ed| ed.set_finish_at(1, at(10, 17))),
    ];
    // Mon 17:00 + 12 eh; Fri 17:00 + 2 edays.
    let windows = [(at(2, 17), 720, 6, Some(5)), (at(6, 17), 2880, 8, None)];
    let mut compared = 0;
    for (start, minutes, code, working_code) in windows {
        for kind in [None, Some(TaskType::FixedWork)] {
            for manual in [false, true] {
                for (name, write) in writes {
                    if name == "manual finish" && !manual {
                        continue;
                    }
                    let case = format!("{name} {code} {kind:?} manual {manual}");
                    let mut elapsed = window_plan(start, minutes, Some(code), kind, manual);
                    // By hand: no working minutes; a Fixed Work task keeps
                    // its work, the others' goes with the duration.
                    let mut working = window_plan(start, 0, working_code, kind, manual);
                    if kind == Some(TaskType::FixedWork) {
                        let mut proj = working.project().clone();
                        proj.assignments[0].work_min = minutes;
                        working = Editor::new(proj);
                    }
                    let want = write(&mut working);
                    assert_eq!(want, Ok(()), "{case}");
                    assert_eq!(write(&mut elapsed), want, "{case}");
                    assert_eq!(facts(&elapsed), facts(&working), "{case}");
                    assert_eq!(elapsed.undo_depth(), 1, "{case}");
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 2 * 2 * (3 + 3 + 1));
    // A milestone toggle there makes the task a milestone.
    let mut ed = window_plan(at(6, 17), 2880, Some(8), None, false);
    ed.toggle_milestone(1).unwrap();
    let t = ed.project().task(1).unwrap();
    assert!(t.milestone);
    assert_eq!((t.duration_min, t.duration_format), (0, None));
}

/// r6 M2: progress there still has nothing to be counted in, so a task with
/// an actual start refuses even a replacing write, changing nothing.
#[test]
fn a_started_elapsed_task_with_no_working_time_refuses_a_duration() {
    let mut ed = window_plan(at(6, 17), 2880, Some(8), None, false);
    let mut proj = ed.project().clone();
    proj.tasks[0].actual_start = Some(at(6, 17));
    ed = Editor::new(proj);
    let before = ed.project().clone();
    assert_eq!(
        ed.set_duration(1, "2d"),
        Err(ELAPSED_NO_WORKING_TIME.to_string())
    );
    assert_eq!(ed.project(), &before);
}

/// r6 M3, M4 as the review traced them: `1d` on `1 eday` from a Monday
/// morning (the minutes the conversion gives) and Ctrl+Delete on `8 ehr`
/// (the stored number) both leave the task in working time.
#[test]
fn a_typed_duration_on_one_elapsed_unit_is_a_change() {
    let mut ed = window_plan(at(2, 8), 1440, Some(8), None, false);
    ed.set_duration(1, "1d").unwrap();
    let t = ed.project().task(1).unwrap();
    assert_eq!((t.duration_min, t.duration_format), (480, None));
    assert_eq!(ed.undo_depth(), 1);
    let mut ed = window_plan(at(2, 8), 480, Some(6), Some(TaskType::FixedWork), false);
    ed.reset_duration(1, 480, true).unwrap();
    let t = ed.project().task(1).unwrap();
    assert_eq!((t.duration_min, t.estimated), (480, Some(true)));
    assert!(!t.elapsed_duration());
    // 8 elapsed hours from 08:00 hold 7 working hours (lunch out): the
    // Fixed Work keeps that work, its units follow the new day.
    let a = &ed.project().assignments[0];
    assert_eq!(a.work_min, 420);
    assert!((a.units - 420.0 / 480.0).abs() < 1e-9, "{}", a.units);
}

/// Task 1 of three elapsed days from Thu 5 March 08:00 (its window holds
/// Thursday and Friday, 960 working minutes), typed `kind`: Alice over all
/// of it, Bob for 24 elapsed hours after a two-hour Delay, Carol for the
/// first 24 elapsed hours. `elapsed`: as stored; else converted by hand,
/// each assignment the working time its own span covers (Alice 960, Bob
/// Thu 10:00 to Fri 10:00 and Carol Thu 08:00 to Fri 08:00, 480 each).
fn thursday_plan(elapsed: bool, kind: Option<TaskType>, manual: bool) -> Editor {
    three_eday_plan(5, elapsed, kind, manual)
}

/// [`thursday_plan`]'s task from 08:00 on March `day` (2, a Monday: a
/// window of 1440 working minutes; 5, a Thursday: 960). Bob's and Carol's
/// spans hold 480 working minutes either way.
fn three_eday_plan(day: u32, elapsed: bool, kind: Option<TaskType>, manual: bool) -> Editor {
    let window = if day == 2 { 1440 } else { 960 };
    let (duration, alice, share) = if elapsed {
        (4320, 4320, 1440)
    } else {
        (window, window, 480)
    };
    let person = |uid: i32, name: &str| Resource {
        uid,
        id: uid,
        name: name.into(),
        kind: ResourceType::Work,
        max_units: 1.0,
        ..Resource::default()
    };
    let assignment = |uid: i32, work_min: i64, delay: Option<i64>| Assignment {
        uid,
        task_uid: 1,
        resource_uid: uid,
        units: 1.0,
        work_min,
        delay,
        ..Assignment::default()
    };
    Editor::new(Project {
        start_date: Some(at(day, 8)),
        tasks: vec![Task {
            uid: 1,
            id: 1,
            name: "E".into(),
            outline_level: 1,
            duration_min: duration,
            duration_format: elapsed.then_some(8),
            task_type: kind,
            manual,
            manual_start: manual.then(|| at(day, 8)),
            manual_duration_min: manual.then_some(duration),
            ..Task::default()
        }],
        resources: vec![person(1, "Alice"), person(2, "Bob"), person(3, "Carol")],
        assignments: vec![
            assignment(1, alice, None),
            assignment(2, share, Some(120 * 10)),
            assignment(3, share, None),
        ],
        ..Project::default()
    })
}

/// Every assignment's units, work, delay, actual work and span, and the
/// task's duration and finish.
fn all_facts(ed: &Editor) -> String {
    let p = ed.project();
    let t = p.task(1).unwrap();
    let mut out = format!(
        "duration {} format {:?} actual {:?} remaining {:?} finish {:?}",
        t.duration_min,
        t.duration_format,
        t.actual_duration_min,
        t.remaining_duration_min,
        ed.disp_finish(1)
    );
    for a in &p.assignments {
        let span = crate::assign::assignment_span(p, ed.schedule(), a);
        out += &format!(
            " | {} {:.6} {} {:?} {:?} ot {:?} {:?} {:?} {span:?}",
            a.uid,
            a.units,
            a.work_min,
            a.delay,
            a.actual_work_min,
            a.overtime_work_min,
            a.actual_start,
            a.actual_finish
        );
    }
    out
}

/// r10: over a window that crosses a weekend, the conversion gives each
/// flat assignment the working time its own span covered, not the window's
/// average (which would give Bob and Carol 320 each): a working write on
/// the elapsed task then equals it on the task converted by hand.
#[test]
fn a_working_write_over_a_weekend_window_equals_it_on_the_working_task() {
    let writes = [
        Entry::DurationMin,
        Entry::DurationMinOfElapsed,
        Entry::TypedConverted,
        Entry::ManualFinish,
        Entry::ActualStart,
        Entry::ActualFinish,
        Entry::ActualDuration,
        Entry::RemainingDuration,
    ];
    let (mut compared, mut refused) = (0, Vec::new());
    for write in writes {
        for kind in [
            None,
            Some(TaskType::FixedWork),
            Some(TaskType::FixedDuration),
        ] {
            for manual in [false, true] {
                if matches!(write, Entry::ManualFinish) && !manual {
                    continue;
                }
                let case = format!("{write:?} {kind:?} manual {manual}");
                let mut working = thursday_plan(false, kind, manual);
                let mut elapsed = thursday_plan(true, kind, manual);
                let want = write.apply(&mut working);
                assert_eq!(write.apply(&mut elapsed), want, "{case}");
                if want.is_err() {
                    refused.push(format!("{write:?}"));
                    continue;
                }
                assert_eq!(all_facts(&elapsed), all_facts(&working), "{case}");
                compared += 1;
            }
        }
    }
    // The shared Actual Finish (Wed 17:00) is before this task's start.
    assert!(refused.iter().all(|w| w == "ActualFinish"), "{refused:?}");
    assert_eq!((compared, refused.len()), (3 * (2 * 6 + 1), 6));
}

/// r10: the conversion alone keeps every assignment where it was: the same
/// start, a finish with no working time between it and the old one (an
/// instant on the clock lands at the end of the working time before it),
/// and none past its task.
#[test]
fn the_conversion_keeps_every_assignment_on_its_span() {
    for kind in [
        None,
        Some(TaskType::FixedWork),
        Some(TaskType::FixedDuration),
    ] {
        for manual in [false, true] {
            let case = format!("{kind:?} manual {manual}");
            let mut ed = thursday_plan(true, kind, manual);
            let spans = |ed: &Editor| -> Vec<(DateTime, DateTime)> {
                let p = ed.project();
                p.assignments
                    .iter()
                    .map(|a| crate::assign::assignment_span(p, ed.schedule(), a).unwrap())
                    .collect()
            };
            let before = spans(&ed);
            ed.elapsed_to_working(0, Write::Progress).unwrap();
            let after = spans(&ed);
            let calendar =
                crate::assign::task_calendar(ed.project(), ed.project().task(1).unwrap());
            let finish = ed.disp_finish(1).unwrap();
            for (k, (old, new)) in before.iter().zip(&after).enumerate() {
                assert_eq!(new.0, old.0, "{case} assignment {k} start");
                let (early, late) = if new.1 <= old.1 {
                    (new.1, old.1)
                } else {
                    (old.1, new.1)
                };
                assert_eq!(
                    crate::schedule::working_minutes_on(&calendar, early, late),
                    0,
                    "{case} assignment {k}: {old:?} -> {new:?}"
                );
                assert!(new.1 <= finish, "{case} assignment {k} past {finish:?}");
            }
        }
    }
    // Actual work recorded on an assignment of an untracked task (as an
    // import can carry) follows its own factor, so Carol stays half done:
    // 720 of 1440 becomes 240 of 480, not the window's 160.
    let mut ed = thursday_plan(true, None, false);
    let mut proj = ed.project().clone();
    proj.assignments[2].actual_work_min = Some(720);
    ed = Editor::new(proj);
    ed.elapsed_to_working(0, Write::Progress).unwrap();
    let carol = &ed.project().assignments[2];
    assert_eq!((carol.work_min, carol.actual_work_min), (480, Some(240)));
}

/// What an r11 row adds to [`three_eday_plan`] (elapsed figures, the
/// working twin's a third: each assignment's span holds a third of its
/// wall-clock minutes in working time).
#[derive(Clone, Copy, Debug)]
enum Extra {
    /// Bob has 6 hours of scheduled overtime.
    Overtime,
    /// Carol's actual work, half of it, entered through `set_assignment`
    /// (which sets her actual start on an untracked task).
    AssignmentActuals,
    /// Carol is done: her actual start and finish (her whole clock span)
    /// and all her work actual.
    ActualFinish,
}

impl Extra {
    fn apply(self, ed: &mut Editor, day: u32, elapsed: bool) {
        let third = |m: i64| if elapsed { m } else { m / 3 };
        match self {
            Extra::Overtime => {
                let mut proj = ed.project().clone();
                proj.assignments[1].overtime_work_min = Some(third(360));
                *ed = Editor::new(proj);
            }
            Extra::AssignmentActuals => {
                let patch = crate::editor::AssignmentPatch {
                    actual_work_min: Some(third(720)),
                    ..Default::default()
                };
                ed.set_assignment(3, patch).unwrap();
            }
            Extra::ActualFinish => {
                let mut proj = ed.project().clone();
                let carol = &mut proj.assignments[2];
                carol.actual_start = Some(at(day, 8));
                carol.actual_finish = Some(at(day + 1, 8));
                carol.actual_work_min = Some(third(1440));
                *ed = Editor::new(proj);
            }
        }
    }
}

/// r11: scheduled overtime, actual work entered on an assignment, and an
/// assignment's actual finish come through a progress write on the elapsed
/// task as on its working twin, from a Monday and over a weekend.
#[test]
fn assignment_progress_and_overtime_convert_by_their_own_spans() {
    let writes = [
        Entry::ActualStart,
        Entry::ActualDuration,
        Entry::RemainingDuration,
    ];
    let mut compared = 0;
    for day in [2, 5] {
        for extra in [
            Extra::Overtime,
            Extra::AssignmentActuals,
            Extra::ActualFinish,
        ] {
            for kind in [
                None,
                Some(TaskType::FixedWork),
                Some(TaskType::FixedDuration),
            ] {
                for write in writes {
                    let case = format!("day {day} {extra:?} {kind:?} {write:?}");
                    let mut working = three_eday_plan(day, false, kind, false);
                    extra.apply(&mut working, day, false);
                    let mut elapsed = three_eday_plan(day, true, kind, false);
                    extra.apply(&mut elapsed, day, true);
                    let want = write.apply(&mut working);
                    assert_eq!(want, Ok(()), "{case}");
                    assert_eq!(write.apply(&mut elapsed), want, "{case}");
                    assert_eq!(all_facts(&elapsed), all_facts(&working), "{case}");
                    let ot = |ed: &Editor| ed.project().assignments[1].overtime_work_min;
                    // A remaining-duration edit changes the duration, which
                    // drops overtime on the working twin too (`set_work`).
                    if matches!(extra, Extra::Overtime)
                        && !matches!(write, Entry::RemainingDuration)
                    {
                        assert_eq!(ot(&elapsed), Some(120), "{case}");
                    }
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 2 * 3 * 3 * 3);
}

/// One elapsed leaf of `minutes` in `format` from the project start
/// `start`, on `calendar`, with the Inspector at 100% for `work` minutes
/// (priced at 60/h, a stored cost, regular work and overtime cost).
fn priced_plan(
    start: DateTime,
    minutes: i64,
    format: Option<u8>,
    work: i64,
    calendar: Calendar,
) -> Editor {
    let mut ed = window_plan(start, minutes, format, None, false);
    let mut proj = ed.project().clone();
    proj.calendars = vec![calendar];
    proj.default_calendar_uid = 1;
    proj.resources[0].standard_rate = crate::model::Rate::parse("60");
    proj.resources[0].overtime_rate = crate::model::Rate::parse("90");
    let a = &mut proj.assignments[0];
    a.work_min = work;
    a.overtime_work_min = Some(60);
    a.regular_work_min = Some(work - 60);
    a.cost = crate::model::Rate::parse("777");
    a.overtime_cost = crate::model::Rate::parse("9000");
    ed = Editor::new(proj);
    ed
}

/// r12 M1, r13: an elapsed task from Fri 17:00 (its window to Mon 17:00
/// holds Monday) with the Inspector's two hours Fri 17:00 to 19:00, no
/// working time. With her actual work entered, a progress write on the
/// task would put it outside working time: refused, the plan as it was.
/// Without, she keeps her work and its price, as a progress write on a
/// working task keeps them, and starts at the next working minute.
#[test]
fn an_assignment_span_with_no_working_time_refuses_or_keeps_its_work() {
    let plan = || priced_plan(at(6, 17), 4320, Some(8), 120, Calendar::standard(1));
    let mut ed = plan();
    let patch = crate::editor::AssignmentPatch {
        actual_work_min: Some(120),
        ..Default::default()
    };
    ed.set_assignment(1, patch).unwrap();
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    assert_eq!(
        ed.set_actual_start(1, Some(at(6, 17))).map(drop),
        Err(ELAPSED_ASSIGNMENT_NO_WORKING_TIME.to_string())
    );
    assert_eq!(ed.project(), &before);
    assert_eq!(ed.undo_depth(), depth);
    // Without actuals: her work as it was, priced whole (her task's start
    // moved to Monday, so the refresh prices her: 60 regular minutes at
    // 60/h and 60 overtime at 90/h), not cut to the window's 480 / 4320.
    let mut ed = plan();
    ed.set_actual_start(1, Some(at(6, 17))).unwrap();
    assert!(!ed.project().task(1).unwrap().elapsed_duration());
    assert_eq!(ed.undo_depth(), 1);
    let a = &ed.project().assignments[0];
    assert_eq!(
        (a.work_min, a.overtime_work_min, a.regular_work_min),
        (120, Some(60), Some(60))
    );
    assert_eq!(
        a.cost.as_ref().map(crate::model::Rate::as_str),
        Some("15000")
    );
    // In working time she starts with her task (whose actual start the
    // write pinned at Fri 17:00) and her two working hours end Mon 10:00,
    // within the task (Mon 17:00).
    let span = crate::assign::assignment_span(ed.project(), ed.schedule(), a);
    assert_eq!(span, Some((at(6, 17), at(9, 10))));
    assert_eq!(ed.disp_finish(1), Some(at(9, 17)));
}

/// r12 M2: a conversion that leaves an assignment's work as it is (a
/// factor of 1: four elapsed hours from Monday 08:00 to 12:00, or any span
/// on a 24-hour calendar) keeps its priced fields, as the same progress
/// write on its working twin does.
#[test]
fn a_conversion_of_factor_one_keeps_the_priced_fields() {
    let all_day = Calendar {
        week: std::array::from_fn(|_| {
            Some(DayWorking {
                times: vec![WorkingTime { from: 0, to: 1440 }],
            })
        }),
        ..Calendar::standard(1)
    };
    let cases = [
        ("4 ehrs from Monday", Calendar::standard(1), 240, 6, Some(5)),
        ("3 edays all day", all_day, 4320, 8, None),
    ];
    for (case, calendar, minutes, code, working_code) in cases {
        let mut elapsed = priced_plan(at(2, 8), minutes, Some(code), minutes, calendar.clone());
        let mut working = priced_plan(at(2, 8), minutes, working_code, minutes, calendar);
        working.set_actual_start(1, Some(at(2, 8))).unwrap();
        elapsed.set_actual_start(1, Some(at(2, 8))).unwrap();
        let fields = |ed: &Editor| {
            let a = &ed.project().assignments[0];
            format!(
                "{} {:?} {:?} {:?} {:?} {:?} {:?}",
                a.work_min,
                a.regular_work_min,
                a.overtime_work_min,
                a.cost,
                a.overtime_cost,
                a.remaining_overtime_work_min,
                a.remaining_overtime_cost
            )
        };
        assert_eq!(fields(&elapsed), fields(&working), "{case}");
        let a = &elapsed.project().assignments[0];
        assert!(a.cost.is_some() && a.regular_work_min.is_some(), "{case}");
        assert!(
            !elapsed.project().task(1).unwrap().elapsed_duration(),
            "{case}"
        );
    }
}
