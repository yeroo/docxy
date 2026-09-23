use super::*;

fn editor() -> Editor {
    let mut p = untitled_project();
    p.tasks = (1..=3)
        .map(|id| Task {
            id,
            uid: id * 10,
            name: format!("Task {id}"),
            duration_min: 480,
            ..Task::default()
        })
        .collect();
    Editor::new(p)
}

fn unchanged(ed: &Editor, p: &Project, history: (usize, usize, bool)) {
    assert_eq!(ed.project(), p);
    assert_eq!((ed.undo_depth(), ed.redo_depth(), ed.dirty()), history);
}

#[test]
fn predecessor_validation_is_atomic_and_replacement_is_one_step() {
    let mut ed = editor();
    ed.rename(10, "other").unwrap();
    ed.undo();
    ed.mark_saved();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    for text in ["99", "1,1", "2FS+NaNd", "2FS+1e30d", "2XX", "2,", "2FS--1d"] {
        assert!(parse_predecessors(text, ed.project()).is_err(), "{text}");
        unchanged(&ed, &before, history);
    }
    let self_link = parse_predecessors("1", ed.project()).unwrap();
    assert!(ed.set_predecessors(10, self_link).is_err());
    assert!(
        ed.set_predecessors(
            10,
            vec![Predecessor {
                uid: 999,
                link: LinkType::FinishStart,
                lag_min: 0
            }]
        )
        .is_err()
    );
    unchanged(&ed, &before, history);
    let preds = parse_predecessors("2SS+2h, 3FF-7m", ed.project()).unwrap();
    ed.set_predecessors(10, preds.clone()).unwrap();
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(
        format_predecessors(ed.project().task(10).unwrap(), ed.project()),
        "2SS+2h, 3FF-7m"
    );
    ed.set_predecessors(10, preds).unwrap();
    assert_eq!(ed.undo_depth(), 1);
    let after = ed.project().clone();
    assert!(ed.undo());
    assert_eq!(ed.project(), &before);
    assert!(ed.redo());
    assert_eq!(ed.project(), &after);
    ed.set_predecessors(10, vec![]).unwrap();
    assert!(ed.project().task(10).unwrap().predecessors.is_empty());
}

#[test]
fn resources_preserve_allocations_and_undo_creation_together() {
    let mut ed = editor();
    ed.set_resources(10, &["Alice".into(), "Bob".into()])
        .unwrap();
    ed.proj.assignments[0].units = 0.5;
    ed.proj.assignments[0].work_min = 123;
    let kept = ed.proj.assignments[0];
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    ed.mark_saved();
    ed.set_resources(10, &["bob".into(), "ALICE".into(), "Alice".into()])
        .unwrap();
    unchanged(&ed, &before, (depth, 0, false));
    ed.set_resources(10, &["alice".into(), "Carol".into(), "carol".into()])
        .unwrap();
    assert_eq!(ed.undo_depth(), depth + 1);
    assert_eq!(ed.proj.assignments[0], kept);
    assert_eq!(ed.proj.assignments.len(), 2);
    assert_eq!(ed.proj.resources.len(), 3);
    ed.undo();
    assert_eq!(ed.project(), &before);
    ed.redo();
    ed.set_resources(10, &[]).unwrap();
    assert!(ed.proj.assignments.is_empty());
}

#[test]
fn resource_id_exhaustion_cannot_partially_create_resources() {
    for resource_limit in [true, false] {
        let mut ed = editor();
        ed.assign_resource(10, "Alice").unwrap();
        if resource_limit {
            ed.proj.resources[0].uid = i32::MAX;
        } else {
            ed.proj.assignments[0].uid = i32::MAX;
        }
        ed.mark_saved();
        let before = ed.project().clone();
        let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
        assert!(
            ed.set_resources(10, &["Bob".into(), "Carol".into()])
                .is_err()
        );
        unchanged(&ed, &before, history);
    }
}

#[test]
fn unchanged_fields_and_constraints_preserve_redo_and_clean_state() {
    let mut ed = editor();
    ed.rename(10, "changed").unwrap();
    ed.undo();
    ed.mark_saved();
    ed.rename(10, "Task 1").unwrap();
    ed.set_duration(10, "8h").unwrap();
    ed.set_constraint_typed(10, ConstraintType::AsSoonAsPossible, None)
        .unwrap();
    assert_eq!(
        (ed.undo_depth(), ed.redo_depth(), ed.dirty()),
        (0, 1, false)
    );
    assert!(
        ed.set_constraint_typed(10, ConstraintType::MustFinishOn, None)
            .is_err()
    );
    assert_eq!(
        (ed.undo_depth(), ed.redo_depth(), ed.dirty()),
        (0, 1, false)
    );
}

#[test]
fn aggregate_duration_and_lag_overflow_reject_before_snapshot() {
    let mut ed = editor();
    let before = ed.project().clone();
    assert!(ed.set_duration_min(10, i64::MAX - 1).is_err());
    for lag_min in [i64::MIN, i64::MAX - 1] {
        assert!(
            ed.set_predecessors(
                10,
                vec![Predecessor {
                    uid: 20,
                    link: LinkType::FinishStart,
                    lag_min
                }]
            )
            .is_err()
        );
    }
    unchanged(&ed, &before, (0, 0, false));
}

#[test]
fn exact_duration_format_preserves_minutes_beyond_float_integer_precision() {
    let p = untitled_project();
    for min in [0, 1, -1, 9_007_199_254_740_993, i64::MAX, i64::MIN] {
        assert_eq!(
            parse_duration(&format_duration_exact(min, &p), &p),
            Some(min)
        );
    }
}

#[test]
fn dates_are_strict_and_finish_uses_effective_calendar() {
    for s in [
        "2026-02-29",
        "2026-02-31",
        "2026-04-31",
        "2026-13-01",
        "26-01-01",
        "2026-1-01",
        "2026-01-01T08:00:00",
        "λ2026-01",
    ] {
        assert!(parse_cell_date(s).is_err(), "{s}");
    }
    assert!(parse_cell_date("2024-02-29").is_ok());
    let mut ed = editor();
    let day = parse_cell_date("2026-01-08").unwrap();
    let finish = day_finish(ed.project(), ed.project().task(10).unwrap(), day).unwrap();
    assert_eq!(finish.minute_of_day(), 17 * 60);
    ed.set_constraint_typed(10, ConstraintType::FinishNoEarlierThan, Some(finish))
        .unwrap();
    assert_eq!(ed.disp_finish(10), Some(finish));
    assert!(
        day_finish(
            ed.project(),
            ed.project().task(10).unwrap(),
            parse_cell_date("2026-01-10").unwrap()
        )
        .is_err()
    );
    let mut cal = Calendar::standard(99);
    cal.week[6] = cal.week[1].clone();
    cal.week[6].times.last_mut().unwrap().to = 18 * 60;
    ed.proj.calendars.push(cal);
    ed.proj.tasks[0].calendar_uid = Some(99);
    let sat = day_finish(
        ed.project(),
        ed.project().task(10).unwrap(),
        parse_cell_date("2026-01-10").unwrap(),
    )
    .unwrap();
    assert_eq!(sat.minute_of_day(), 18 * 60);
    ed.set_constraint_typed(10, ConstraintType::FinishNoEarlierThan, Some(sat))
        .unwrap();
    assert_eq!(ed.disp_finish(10), Some(sat));
    ed.proj.tasks[0].calendar_uid = Some(12345);
    assert_eq!(
        day_finish(ed.project(), ed.project().task(10).unwrap(), day)
            .unwrap()
            .minute_of_day(),
        17 * 60
    );
    ed.set_constraint_typed(10, ConstraintType::StartNoEarlierThan, Some(day))
        .unwrap();
    assert_eq!(ed.disp_start(10).unwrap().minute_of_day(), 8 * 60);
}

#[test]
fn all_corpus_predecessors_and_synthetic_lags_round_trip() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/mspdi");
    for file in std::fs::read_dir(root).unwrap() {
        let path = file.unwrap().path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let p = crate::mspdi::read_mspdi(&std::fs::read_to_string(path).unwrap()).unwrap();
        for t in &p.tasks {
            assert_eq!(
                parse_predecessors(&format_predecessors(t, &p), &p).unwrap(),
                t.predecessors
            );
        }
    }
    let mut ed = editor();
    for link in [
        LinkType::FinishStart,
        LinkType::FinishFinish,
        LinkType::StartStart,
        LinkType::StartFinish,
    ] {
        for lag_min in [-481, -120, -1, 0, 1, 120, 480, 960] {
            let preds = vec![Predecessor {
                uid: 20,
                link,
                lag_min,
            }];
            ed.proj.tasks[0].predecessors = preds.clone();
            assert_eq!(
                parse_predecessors(&format_predecessors(&ed.proj.tasks[0], &ed.proj), &ed.proj)
                    .unwrap(),
                preds
            );
        }
    }
    for s in ["NaN", "inf", "-inf", "1e50d", "9223372036854775808m"] {
        assert_eq!(parse_duration(s, ed.project()), None);
    }
}

#[test]
fn finish_cell_uses_the_six_day_corpus_calendar() {
    let p = crate::mspdi::read_mspdi(include_str!(
        "../../../../corpus/mspdi/12-calendar-6day.xml"
    ))
    .unwrap();
    let mut ed = Editor::new(p);
    let saturday = parse_cell_date("2026-03-14").unwrap();
    let end = day_finish(ed.project(), ed.project().task(1).unwrap(), saturday).unwrap();
    ed.set_constraint_typed(1, ConstraintType::FinishNoEarlierThan, Some(end))
        .unwrap();
    assert_eq!(ed.disp_finish(1), Some(end));
    assert_eq!(end.minute_of_day(), 17 * 60);
    ed.undo();
    assert_eq!(ed.disp_finish(1).unwrap().parts().day, 7);
}
