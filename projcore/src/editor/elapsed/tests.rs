use super::*;

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 3, day, hour, 0)
}

/// Task 1 of three days from Mon 2 March 08:00, typed `kind`, with Alice at
/// 100% on it. `elapsed`: 72 elapsed hours (format 8) and Alice's work
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
            work_min: m(1440),
            actual_work_min: started.then(|| m(480)),
            ..Assignment::default()
        }],
        ..Project::default()
    })
}

/// Every entry point that writes working minutes over a task.
#[derive(Clone, Copy, Debug)]
enum Write {
    DurationMin,
    UpdateTask,
    ManualFinish,
    ActualStart,
    ActualFinish,
    ActualDuration,
    PercentSynced,
    RemainingDuration,
}

impl Write {
    fn apply(self, ed: &mut Editor) -> Result<(), String> {
        match self {
            Write::DurationMin => ed.set_duration_min(1, 2000, false),
            Write::UpdateTask => ed.update_task(
                1,
                TaskPatch {
                    duration_min: Some(2000),
                    ..TaskPatch::default()
                },
            ),
            Write::ManualFinish => ed.set_finish_at(1, at(5, 17)),
            Write::ActualStart => ed.set_actual_start(1, Some(at(3, 8))).map(drop),
            Write::ActualFinish => ed.set_actual_finish(1, Some(at(4, 17))).map(drop),
            Write::ActualDuration => ed.set_actual_duration(1, 600).map(drop),
            Write::PercentSynced => ed.set_percent_complete_synced(1, 60).map(drop),
            Write::RemainingDuration => ed.set_remaining_duration(1, 1000).map(drop),
        }
    }
}

/// What a write decides on task 1 and Alice's assignment (the refresh's own
/// derived fields left out: it runs against different "before" states).
fn facts(ed: &Editor) -> String {
    let t = ed.project().task(1).unwrap();
    let a = &ed.project().assignments[0];
    format!(
        "duration {} format {:?} estimated {:?} manual {:?} pin {:?}..{:?} \
         actual {:?}..{:?} {:?} remaining {:?} percent {:?} constraint {:?} \
         | units {:.6} work {} actual work {:?} overtime {:?}",
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

/// r5: every write of working minutes over an elapsed task gives what the
/// same write gives on that task converted to working time by hand, for
/// every task type, manual or not, unstarted or in progress with actual
/// work (r5 M1: a Fixed Work task's units, M2: the actual work), as one
/// undo step that restores everything; a refused write changes nothing.
#[test]
fn a_working_write_on_an_elapsed_task_equals_it_on_the_working_task() {
    let writes = [
        Write::DurationMin,
        Write::UpdateTask,
        Write::ManualFinish,
        Write::ActualStart,
        Write::ActualFinish,
        Write::ActualDuration,
        Write::PercentSynced,
        Write::RemainingDuration,
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
                    if matches!(write, Write::ManualFinish) && !manual
                        || matches!(write, Write::PercentSynced) && !started
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
    assert_eq!(compared, 3 * (2 * 2 * 6 + 2 + 2), "every case wrote");
}

/// The conversion alone: duration, actual and remaining durations, the pin,
/// the assignment's work and actual work by the window's factor (72 elapsed
/// hours from Mon 08:00 hold 24 working hours), units kept, the format to
/// days; and it changes nothing when the edit after it does nothing.
#[test]
fn the_conversion_scales_the_whole_task_by_its_window() {
    let mut ed = plan(true, Some(TaskType::FixedWork), true, true);
    ed.elapsed_to_working(0).unwrap();
    let want = plan(false, Some(TaskType::FixedWork), true, true);
    assert_eq!(facts(&ed), facts(&want));
    // An edit that changes nothing rolls its conversion back.
    let mut ed = plan(true, None, false, true);
    let before = ed.project().clone();
    let i = ed.index(1).unwrap();
    assert_eq!(ed.in_working_time(i, |_| Ok(false)), Ok(false));
    assert_eq!(ed.project(), &before);
    assert_eq!(ed.undo_depth(), 0);
}
