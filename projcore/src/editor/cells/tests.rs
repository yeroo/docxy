use super::*;
use crate::model::{AssignmentBaseline, Rate};

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
    let kept = ed.proj.assignments[0].clone();
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    ed.mark_saved();
    // Retained allocations survive when their cell text is unchanged.
    ed.set_resources(10, &["bob".into(), "ALICE[50%]".into(), "Alice".into()])
        .unwrap();
    unchanged(&ed, &before, (depth, 0, false));
    ed.set_resources(10, &["alice [50%]".into(), "Carol".into(), "carol".into()])
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
fn resource_names_edits_keep_assignment_progress() {
    let mut ed = editor();
    ed.set_resources(10, &["Alice".into(), "Bob".into()])
        .unwrap();
    for a in &mut ed.proj.assignments {
        a.percent_work_complete = Some(50);
        a.actual_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        a.stop = Some(DateTime::from_ymd_hm(2026, 3, 2, 12, 0));
        a.resume = Some(DateTime::from_ymd_hm(2026, 3, 2, 13, 0));
        a.actual_work_min = Some(240);
        a.remaining_work_min = Some(240);
        a.actual_cost = Rate::parse("200");
        a.work_variance = Rate::parse("0.0");
        a.set_baseline_slot(AssignmentBaseline {
            number: 0,
            work_min: Some(480),
            cost: Rate::parse("400"),
            ..AssignmentBaseline::default()
        });
    }
    let tracked = ed.proj.assignments.clone();
    let before = ed.project().clone();
    ed.mark_saved();
    let depth = ed.undo_depth();
    // The cell's own text changes nothing.
    let text = format_resource_names(&ed.proj, 10);
    let names: Vec<String> = text.split(", ").map(String::from).collect();
    ed.set_resources(10, &names).unwrap();
    unchanged(&ed, &before, (depth, 0, false));
    // Adding a resource leaves the tracked assignments as they were.
    ed.set_resources(10, &["Alice".into(), "Bob".into(), "Carol".into()])
        .unwrap();
    assert_eq!(ed.proj.assignments[..2], tracked);
    assert_eq!(ed.proj.assignments[2].percent_work_complete, None);
    // A units change rewrites units and work only.
    ed.set_resources(10, &["Alice[50%]".into(), "Bob".into()])
        .unwrap();
    let alice = &ed.proj.assignments[0];
    assert_eq!(alice.units, 0.5);
    assert_eq!(
        Assignment {
            units: tracked[0].units,
            work_min: tracked[0].work_min,
            ..alice.clone()
        },
        tracked[0]
    );
    assert_eq!(ed.proj.assignments[1], tracked[1]);
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
fn scheduling_range_reserves_room_for_dated_start_indices() {
    let mut ed = editor();
    // Keep the old aggregate-only preflight below i64::MAX, so rejection must
    // come from reserving room for the later start index rather than task sums.
    for task in &mut ed.proj.tasks {
        task.duration_min = 0;
    }
    ed.set_constraint_typed(
        10,
        ConstraintType::StartNoEarlierThan,
        Some(parse_cell_date("2027-01-04").unwrap()),
    )
    .unwrap();
    ed.rename(10, "temporary").unwrap();
    ed.undo();
    ed.mark_saved();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    let minutes = 9_223_372_036_854_679_000;
    assert!(
        ed.set_duration(10, "9223372036854679000m")
            .unwrap_err()
            .contains("scheduling range")
    );
    assert!(ed.set_duration_min(10, minutes).is_err());
    assert!(
        ed.update_task(
            10,
            TaskPatch {
                name: Some("must not rename".into()),
                duration_min: Some(minutes),
                ..TaskPatch::default()
            }
        )
        .is_err()
    );
    for lag_min in [minutes, -minutes] {
        assert!(
            ed.set_predecessors(
                20,
                vec![Predecessor {
                    uid: 10,
                    link: LinkType::FinishStart,
                    lag_min,
                }]
            )
            .unwrap_err()
            .contains("scheduling range")
        );
    }
    unchanged(&ed, &before, history);
}

#[test]
fn scheduling_range_accounts_for_time_before_the_anchor() {
    let mut ed = editor();
    for task in &mut ed.proj.tasks {
        task.duration_min = 0;
    }
    // A continuous calendar can use the entire 100-year backward budget as
    // well as the forward budget. The old reserve only covered one direction.
    for day in ed.proj.calendars[0].week.iter_mut().flatten() {
        day.times = vec![crate::model::WorkingTime { from: 0, to: 1440 }];
    }
    // Keep an SF leaf link so this project needs a backward horizon.
    ed.set_predecessors(
        20,
        vec![Predecessor {
            uid: 30,
            link: LinkType::StartFinish,
            lag_min: 0,
        }],
    )
    .unwrap();
    ed.set_constraint_typed(
        10,
        ConstraintType::StartNoEarlierThan,
        Some(parse_cell_date("2027-01-04").unwrap()),
    )
    .unwrap();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    let old_reserve = (366_i64 * 100 + 1) * 1440 + 200 * 480 + 480;
    assert!(ed.set_duration_min(10, i64::MAX - old_reserve).is_err());
    unchanged(&ed, &before, history);
}

#[test]
fn resource_tokens_preserve_significant_whitespace_and_prefer_exact_assignments() {
    for (stored, token) in [
        ("Alice ", "Alice [50%]"),
        (" Alice", " alice[50%]"),
        (" Alice ", " ALICE [50%] "),
    ] {
        let mut ed = editor();
        ed.assign_resource(10, "Alice").unwrap();
        ed.proj.resources[0].name = stored.into();
        ed.proj.assignments[0].units = 0.5;
        ed.proj.assignments[0].work_min = 123;
        let retained = ed.proj.assignments[0].clone();
        let before = ed.project().clone();
        let depth = ed.undo_depth();
        ed.set_resources(10, &[token.into(), " Bob ".into()])
            .unwrap();
        assert!(ed.proj.assignments.contains(&retained));
        assert_eq!(ed.proj.resources.len(), 2);
        assert_eq!(ed.proj.resources[0].name, stored);
        assert_eq!(ed.proj.resources[1].name, "Bob");
        assert_eq!(ed.undo_depth(), depth + 1);
        ed.undo();
        ed.mark_saved();
        ed.set_resources(10, &[token.into()]).unwrap();
        unchanged(&ed, &before, (depth, 1, false));
    }
    let mut ed = editor();
    ed.set_resources(10, &["Alice".into(), "Second".into()])
        .unwrap();
    ed.proj.resources[1].name = "ALICE".into();
    let retained = ed.proj.assignments.clone();
    ed.set_resources(10, &["Alice".into(), "ALICE".into(), "Bob".into()])
        .unwrap();
    assert!(retained.iter().all(|a| ed.proj.assignments.contains(a)));
}

#[test]
fn negative_duration_rejection_is_atomic_for_all_update_entry_points() {
    let mut ed = editor();
    ed.rename(10, "temporary").unwrap();
    ed.undo();
    ed.mark_saved();
    let before = ed.project().clone();
    assert!(ed.add_task(None, "must not append", -1440).is_err());
    assert!(ed.add_task(Some(10), "must not insert", -1).is_err());
    assert!(ed.set_duration(10, "-3d").is_err());
    assert!(ed.set_duration_min(10, -1).is_err());
    assert!(
        ed.update_task(
            10,
            TaskPatch {
                name: Some("must not rename".into()),
                duration_min: Some(-1),
                ..TaskPatch::default()
            }
        )
        .is_err()
    );
    unchanged(&ed, &before, (0, 1, false));
    // Lead/negative lag remains valid and is independent of duration validation.
    ed.set_predecessors(10, parse_predecessors("2FS-1h", ed.project()).unwrap())
        .unwrap();
}

#[test]
fn duplicate_resource_names_preserve_the_assigned_identity_or_reject_ambiguity() {
    let mut ed = editor();
    ed.assign_resource(20, "Alice").unwrap();
    ed.assign_resource(10, "Second Alice").unwrap();
    ed.proj.resources[1].name = "ALICE".into();
    ed.proj.assignments[1].units = 0.5;
    ed.proj.assignments[1].work_min = 123;
    let retained = ed.proj.assignments[1].clone();
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    ed.set_resources(10, &["Alice[50%]".into(), "Bob".into()])
        .unwrap();
    assert!(ed.proj.assignments.contains(&retained));
    assert!(
        !ed.proj
            .assignments
            .iter()
            .any(|a| a.task_uid == 10 && a.resource_uid == 1)
    );
    assert_eq!(ed.undo_depth(), depth + 1);
    ed.undo();
    ed.mark_saved();
    unchanged(&ed, &before, (depth, 1, false));
    // No assigned match: both Alices are ambiguous, even after staging another name.
    assert!(
        ed.set_resources(30, &["Staged".into(), "alice".into()])
            .unwrap_err()
            .contains("ambiguous")
    );
    unchanged(&ed, &before, (depth, 1, false));
    // One assigned match wins even for a text-unchanged no-op, preserving redo.
    ed.set_resources(10, &["alice[50%]".into()]).unwrap();
    unchanged(&ed, &before, (depth, 1, false));
    // assign_resource retains its existing first-match behavior; two assigned matches
    // with neither spelling an exact match must still be rejected.
    ed.assign_resource(10, "Alice").unwrap();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    assert!(
        ed.set_resources(10, &["alice".into(), "Bob".into()])
            .unwrap_err()
            .contains("ambiguous")
    );
    unchanged(&ed, &before, history);
}

#[test]
fn assignment_entry_points_share_sparse_id_allocation_and_preflight() {
    let mut base = editor();
    base.assign_resource(10, "Alice").unwrap();
    base.proj.resources[0].id = 90;
    base.proj.resources[0].uid = 40;
    base.proj.assignments[0].resource_uid = 40;
    let mut single = Editor::new(base.project().clone());
    let mut replace = Editor::new(base.project().clone());
    single.assign_resource(10, "Bob").unwrap();
    replace
        .set_resources(10, &["Alice".into(), "Bob".into()])
        .unwrap();
    assert_eq!(single.project(), replace.project());
    assert_eq!(
        (
            single.project().resources[1].id,
            single.project().resources[1].uid
        ),
        (91, 41)
    );
    for resource_ids in [true, false] {
        let mut ed = editor();
        ed.assign_resource(10, "Alice").unwrap();
        if resource_ids {
            ed.proj.resources[0].id = i32::MAX;
        } else {
            ed.proj.assignments[0].uid = i32::MAX;
        }
        ed.mark_saved();
        let before = ed.project().clone();
        assert!(ed.assign_resource(10, "Bob").is_err());
        unchanged(&ed, &before, (1, 0, false));
        assert!(ed.set_resources(10, &["Bob".into()]).is_err());
        unchanged(&ed, &before, (1, 0, false));
    }
}

#[test]
fn constraint_abbreviations_match_all_constraint_codes_and_hints() {
    for (code, expected) in ["ASAP", "ALAP", "MSO", "MFO", "SNET", "SNLT", "FNET", "FNLT"]
        .into_iter()
        .enumerate()
    {
        let constraint = ConstraintType::from_code(code as i64).unwrap();
        assert_eq!(constraint.abbrev(), expected);
        let task = Task {
            constraint,
            ..Task::default()
        };
        assert_eq!(
            constraint_hint(&task),
            if code == 0 { "" } else { expected }
        );
    }
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
    cal.week[6].as_mut().unwrap().times.last_mut().unwrap().to = 18 * 60;
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

fn manual_editor() -> Editor {
    let mut ed = editor();
    for task in &mut ed.proj.tasks {
        task.manual = true;
        task.manual_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
    }
    // The file's own finish for task 10, stale once the duration changes.
    ed.proj.tasks[0].stored_finish = Some(DateTime::from_ymd_hm(2026, 1, 5, 17, 0));
    ed.proj.tasks[0].manual_finish = Some(DateTime::from_ymd_hm(2026, 1, 5, 17, 0));
    ed.reschedule();
    ed
}

fn mspdi(date: Option<DateTime>) -> Option<String> {
    date.map(DateTime::to_mspdi)
}

#[test]
fn new_tasks_follow_the_plans_default_mode() {
    let mut ed = editor();
    let at = ed.add_task(None, "auto", 480).unwrap();
    assert!(!ed.project().tasks[at].manual);
    assert_eq!(ed.project().tasks[at].manual_start, None);

    ed.proj.new_tasks_are_manual = true;
    let at = ed.add_task(None, "manual", 960).unwrap();
    let task = &ed.project().tasks[at];
    assert!(task.manual);
    assert_eq!(mspdi(task.manual_start), mspdi(ed.project().start_date));
    assert_eq!(task.manual_duration_min, Some(960));
    // Without a project start, use the anchor the schedule actually uses.
    ed.proj.start_date = None;
    ed.reschedule();
    let anchor = ed.schedule().project_start;
    let at = ed.add_task(None, "undated", 480).unwrap();
    assert_eq!(ed.project().tasks[at].manual_start, Some(anchor));
}

#[test]
fn duration_edit_on_a_manual_task_keeps_its_start_and_moves_its_finish() {
    let mut ed = manual_editor();
    ed.set_duration(10, "2d").unwrap();
    let task = ed.project().task(10).unwrap();
    assert_eq!(
        mspdi(task.manual_start).unwrap(),
        "2026-01-05T08:00:00".to_string()
    );
    assert_eq!(task.manual_finish, None);
    assert_eq!(task.manual_duration_min, Some(960));
    // Not the stale stored finish: the finish follows the new duration.
    assert_eq!(mspdi(ed.disp_finish(10)).unwrap(), "2026-01-06T17:00:00");
}

#[test]
fn typed_start_moves_a_manual_task_without_a_constraint() {
    let mut ed = manual_editor();
    ed.set_start(10, parse_cell_date("2026-01-08").unwrap())
        .unwrap();
    let task = ed.project().task(10).unwrap();
    assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
    assert_eq!(task.manual_finish, None);
    assert_eq!(task.duration_min, 480);
    assert_eq!(mspdi(ed.disp_start(10)).unwrap(), "2026-01-08T08:00:00");
    assert_eq!(mspdi(ed.disp_finish(10)).unwrap(), "2026-01-08T17:00:00");
    // Undo restores the pinned finish in the same step.
    ed.undo();
    assert_eq!(
        mspdi(ed.project().task(10).unwrap().manual_finish).unwrap(),
        "2026-01-05T17:00:00"
    );
}

#[test]
fn typed_finish_sets_a_manual_tasks_duration_without_a_constraint() {
    let mut ed = manual_editor();
    ed.set_finish(20, parse_cell_date("2026-01-07").unwrap())
        .unwrap();
    let task = ed.project().task(20).unwrap();
    assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
    assert_eq!(mspdi(task.manual_finish).unwrap(), "2026-01-07T17:00:00");
    assert_eq!(task.duration_min, 1440);
    assert_eq!(task.manual_duration_min, Some(1440));
    assert_eq!(mspdi(ed.disp_finish(20)).unwrap(), "2026-01-07T17:00:00");

    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    assert!(
        ed.set_finish(20, parse_cell_date("2026-01-02").unwrap())
            .is_err()
    );
    unchanged(&ed, &before, history);
}

#[test]
fn typed_finish_counts_working_time_on_the_tasks_own_calendar() {
    let mut ed = manual_editor();
    let mut six_day = Calendar::standard(7);
    six_day.week[6] = six_day.week[1].clone();
    ed.proj.calendars.push(six_day);
    ed.proj.tasks[1].calendar_uid = Some(7);
    // Friday 2026-01-09 to Monday 2026-01-12, with Saturday working.
    ed.set_start(20, parse_cell_date("2026-01-09").unwrap())
        .unwrap();
    ed.set_finish(20, parse_cell_date("2026-01-12").unwrap())
        .unwrap();
    assert_eq!(ed.project().task(20).unwrap().duration_min, 1440);
}

#[test]
fn typed_dates_on_auto_tasks_still_set_constraints() {
    let mut ed = editor();
    ed.set_start(10, parse_cell_date("2026-01-08").unwrap())
        .unwrap();
    let task = ed.project().task(10).unwrap();
    assert_eq!(task.constraint, ConstraintType::StartNoEarlierThan);
    assert_eq!(task.manual_start, None);
    ed.set_finish(20, parse_cell_date("2026-01-08").unwrap())
        .unwrap();
    let task = ed.project().task(20).unwrap();
    assert_eq!(task.constraint, ConstraintType::FinishNoEarlierThan);
    assert_eq!(mspdi(task.constraint_date).unwrap(), "2026-01-08T17:00:00");
}

/// What a save writes for a task: its Start/Finish and ManualStart.
fn saved(ed: &Editor, uid: i32) -> (Option<String>, Option<String>, Option<String>) {
    let back = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
    let task = back.task(uid).unwrap();
    (
        mspdi(task.stored_start),
        mspdi(task.stored_finish),
        mspdi(task.manual_start),
    )
}

fn assert_saved_consistently(ed: &Editor, uid: i32) {
    let (start, finish, manual_start) = saved(ed, uid);
    assert!(manual_start.is_some());
    assert_eq!(start, manual_start, "Start must equal ManualStart");
    assert_eq!(
        finish,
        mspdi(ed.schedule().get(uid).map(|r| r.early_finish))
    );
}

#[test]
fn edited_manual_tasks_save_start_and_finish_matching_their_pinned_dates() {
    let mut ed = manual_editor();
    for task in &mut ed.proj.tasks {
        // As read from a file: Start/Finish of the original plan.
        task.stored_start = task.manual_start;
        task.stored_finish = Some(DateTime::from_ymd_hm(2026, 1, 5, 17, 0));
    }
    ed.set_start(10, parse_cell_date("2026-01-08").unwrap())
        .unwrap();
    assert_saved_consistently(&ed, 10);
    assert_eq!(saved(&ed, 10).1.unwrap(), "2026-01-08T17:00:00");

    ed.set_finish(20, parse_cell_date("2026-01-07").unwrap())
        .unwrap();
    assert_saved_consistently(&ed, 20);
    assert_eq!(saved(&ed, 20).1.unwrap(), "2026-01-07T17:00:00");

    ed.set_duration(30, "3d").unwrap();
    assert_saved_consistently(&ed, 30);
    assert_eq!(saved(&ed, 30).1.unwrap(), "2026-01-07T17:00:00");

    ed.proj.new_tasks_are_manual = true;
    let at = ed.add_task(None, "new", 480).unwrap();
    let uid = ed.project().tasks[at].uid;
    assert_saved_consistently(&ed, uid);

    // Undo returns the file's own dates in the same step.
    ed.undo();
    ed.undo();
    assert_eq!(saved(&ed, 30).1.unwrap(), "2026-01-05T17:00:00");
}

#[test]
fn unedited_manual_tasks_save_exactly_what_was_read() {
    let ed = manual_editor();
    let task = ed.project().task(10).unwrap();
    assert_eq!(
        (saved(&ed, 10).0, saved(&ed, 10).1),
        (mspdi(task.stored_start), mspdi(task.stored_finish))
    );
}

#[test]
fn manual_finish_on_a_non_working_day_is_accepted_like_a_start() {
    let mut ed = manual_editor();
    // Saturday 2026-01-10: the day's end defaults to 17:00.
    ed.set_finish(20, parse_cell_date("2026-01-10").unwrap())
        .unwrap();
    let task = ed.project().task(20).unwrap();
    assert_eq!(mspdi(task.manual_finish).unwrap(), "2026-01-10T17:00:00");
    // Monday to Friday is five working days; Saturday adds none.
    assert_eq!(task.duration_min, 2400);
    // An auto task's FNET still needs a working day.
    let mut auto = editor();
    assert!(
        auto.set_finish(20, parse_cell_date("2026-01-10").unwrap())
            .is_err()
    );
}

#[test]
fn manual_dates_outside_the_scheduling_range_are_rejected() {
    let mut ed = manual_editor();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    for text in ["2200-01-01", "1026-03-02"] {
        let day = parse_cell_date(text).unwrap();
        assert!(ed.set_start(10, day).is_err(), "{text}");
        assert!(ed.set_finish(10, day).is_err(), "{text}");
        unchanged(&ed, &before, history);
    }
}

#[test]
fn manual_summary_is_not_pinned_and_keeps_its_file_dates() {
    let mut ed = editor();
    let file_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
    let file_finish = Some(DateTime::from_ymd_hm(2026, 1, 7, 17, 0));
    {
        let summary = &mut ed.proj.tasks[0];
        summary.manual = true;
        summary.manual_start = Some(DateTime::from_ymd_hm(2026, 3, 2, 8, 0));
        summary.stored_start = file_start;
        summary.stored_finish = file_finish;
    }
    ed.proj.tasks[1].outline_level = 2;
    ed.reschedule();
    assert!(ed.project().tasks[0].summary);
    assert_eq!(ed.project().tasks[0].pinned_dates(), None);
    // The summary rolls up from its child, not from its ManualStart.
    assert_eq!(
        mspdi(ed.disp_start(10)).unwrap(),
        "2026-01-05T08:00:00".to_string()
    );
    ed.rename(10, "Phase").unwrap();
    assert_eq!(
        (saved(&ed, 10).0, saved(&ed, 10).1),
        (mspdi(file_start), mspdi(file_finish))
    );
}

#[test]
fn renaming_or_indenting_a_manual_task_keeps_its_file_dates() {
    let mut ed = manual_editor();
    // Project's finish includes a holiday projcore does not model.
    let file_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
    let file_finish = Some(DateTime::from_ymd_hm(2026, 1, 6, 17, 0));
    ed.proj.tasks[1].stored_start = file_start;
    ed.proj.tasks[1].stored_finish = file_finish;
    ed.rename(20, "renamed").unwrap();
    ed.update_task(
        20,
        TaskPatch {
            level: Some(2),
            ..TaskPatch::default()
        },
    )
    .unwrap();
    assert_eq!(
        (saved(&ed, 20).0, saved(&ed, 20).1),
        (mspdi(file_start), mspdi(file_finish))
    );
    // A duration edit does restamp.
    ed.set_duration(20, "2d").unwrap();
    assert_saved_consistently(&ed, 20);
}

#[test]
fn repeating_a_manual_tasks_duration_keeps_its_pinned_finish() {
    let mut ed = manual_editor();
    let before = ed.project().task(10).unwrap().clone();
    assert!(before.manual_finish.is_some());
    let saved_before = saved(&ed, 10);
    // As projctl's task.set sends it: a rename plus the current duration.
    ed.update_task(
        10,
        TaskPatch {
            name: Some("renamed".into()),
            duration_min: Some(before.duration_min),
            ..TaskPatch::default()
        },
    )
    .unwrap();
    let task = ed.project().task(10).unwrap();
    assert_eq!(task.manual_finish, before.manual_finish);
    assert_eq!(task.manual_duration_min, before.manual_duration_min);
    assert_eq!(saved(&ed, 10), saved_before);
}

/// Task 10 lasts two days and task 30 is a milestone; resource 1 is `Bob`.
fn with_bob(kind: ResourceType, max_units: f64) -> Editor {
    let mut ed = editor();
    ed.proj.tasks[0].duration_min = 960;
    ed.proj.tasks[2].duration_min = 0;
    ed.proj.resources.push(Resource {
        uid: 1,
        id: 1,
        name: "Bob".into(),
        kind,
        max_units,
        ..Resource::default()
    });
    Editor::new(ed.proj)
}

fn allocation(ed: &Editor, task: i32, resource: i32) -> (f64, i64) {
    let a = ed
        .proj
        .assignments
        .iter()
        .find(|a| a.task_uid == task && a.resource_uid == resource)
        .unwrap();
    (a.units, a.work_min)
}

fn resource_uid(ed: &Editor, name: &str) -> i32 {
    ed.proj
        .resources
        .iter()
        .find(|r| r.name == name)
        .unwrap()
        .uid
}

#[test]
fn both_entry_points_assign_work_resources_at_capped_max_units() {
    use ResourceType::{Cost, Material, Work};
    for (kind, max_units, units) in [
        (Work, 0.5, 0.5),
        (Work, 3.0, 1.0),
        (Work, 1.0, 1.0),
        (Work, 0.0, 1.0),
        (Work, -0.5, 1.0),
        (Work, f64::NAN, 1.0),
        (Work, f64::INFINITY, 1.0),
        (Material, 0.5, 1.0),
        (Cost, 0.5, 1.0),
    ] {
        for cell in [false, true] {
            let mut ed = with_bob(kind, max_units);
            for task in [10, 30] {
                if cell {
                    ed.set_resources(task, &["bob".into()]).unwrap();
                } else {
                    assert_eq!(
                        ed.assign_resource(task, "bob").unwrap(),
                        AssignOutcome::Assigned
                    );
                }
            }
            let case = format!("{kind:?} {max_units} cell={cell}");
            assert_eq!(
                allocation(&ed, 10, 1),
                (units, (960. * units) as i64),
                "{case}"
            );
            assert_eq!(allocation(&ed, 30, 1), (units, 0), "{case}");
        }
    }
    // A resource created by typing a new name is a 100% work resource.
    let mut ed = with_bob(Work, 0.5);
    ed.assign_resource(10, "Dan").unwrap();
    ed.set_resources(20, &["Eve".into()]).unwrap();
    assert_eq!(allocation(&ed, 10, resource_uid(&ed, "Dan")), (1.0, 960));
    assert_eq!(allocation(&ed, 20, resource_uid(&ed, "Eve")), (1.0, 480));
}

#[test]
fn resource_names_show_work_units_that_are_not_100_percent() {
    for (units, text) in [
        (0.5, "50%"),
        (1. / 3., "33.33%"),
        (0.125, "12.5%"),
        (1.5, "150%"),
        (1.0, "100%"),
        (0.0, "0%"),
    ] {
        assert_eq!(format_units(units), text);
    }
    let mut ed = with_bob(ResourceType::Work, 0.5);
    ed.proj.resources.push(Resource {
        uid: 2,
        id: 2,
        name: "Cement".into(),
        kind: ResourceType::Material,
        max_units: 1.0,
        ..Resource::default()
    });
    ed.proj.resources.push(Resource {
        uid: 3,
        id: 3,
        name: "Pool".into(),
        max_units: 3.0,
        ..Resource::default()
    });
    ed.set_resources(10, &["Bob".into(), "Cement[50%]".into(), "Pool".into()])
        .unwrap();
    // Explicit units apply to any kind; only work resources show them.
    assert_eq!(allocation(&ed, 10, 2), (0.5, 480));
    assert_eq!(
        format_resource_names(&ed.proj, 10),
        "Bob[50%], Cement, Pool"
    );
    ed.proj.assignments[2].units = 1.0 + 1e-12;
    assert_eq!(
        format_resource_names(&ed.proj, 10),
        "Bob[50%], Cement, Pool"
    );
    assert_eq!(format_resource_names(&ed.proj, 20), "");
}

#[test]
fn bracketed_units_round_trip_through_the_resource_names_text() {
    let mut ed = with_bob(ResourceType::Work, 0.5);
    // Explicit units on new assignments, including over-allocation.
    ed.set_resources(10, &["Bob[150%]".into(), " Carol [25 %] ".into()])
        .unwrap();
    assert_eq!(ed.undo_depth(), 1);
    assert_eq!(allocation(&ed, 10, 1), (1.5, 1440));
    assert_eq!(allocation(&ed, 10, resource_uid(&ed, "Carol")), (0.25, 240));
    assert!(ed.proj.resources.iter().all(|r| !r.name.contains('[')));
    assert_eq!(format_resource_names(&ed.proj, 10), "Bob[150%], Carol[25%]");
    // Committing the shown text is a no-op, even for units it rounds.
    ed.proj.assignments[0].units = 1. / 3.;
    ed.proj.assignments[0].work_min = 123;
    ed.mark_saved();
    let before = ed.project().clone();
    let text = format_resource_names(&ed.proj, 10);
    assert_eq!(text, "Bob[33.33%], Carol[25%]");
    let tokens: Vec<String> = text.split(',').map(str::to_owned).collect();
    ed.set_resources(10, &tokens).unwrap();
    unchanged(&ed, &before, (1, 0, false));
    // Different units: one undo step, work rescaled from the duration.
    ed.set_resources(10, &["Bob[50%]".into(), "Carol[25%]".into()])
        .unwrap();
    assert_eq!(allocation(&ed, 10, 1), (0.5, 480));
    assert_eq!(ed.undo_depth(), 2);
    ed.undo();
    assert_eq!(ed.project(), &before);
    ed.redo();
    // Deleting a work bracket means 100%; a non-work bare name keeps its units.
    ed.proj.resources.push(Resource {
        uid: 9,
        id: 9,
        name: "Cement".into(),
        kind: ResourceType::Material,
        max_units: 1.0,
        ..Resource::default()
    });
    ed.set_resources(10, &["Bob[50%]".into(), "Cement[40%]".into()])
        .unwrap();
    ed.set_resources(10, &["Bob".into(), "Cement".into()])
        .unwrap();
    assert_eq!(allocation(&ed, 10, 1), (1.0, 960));
    assert_eq!(allocation(&ed, 10, 9), (0.4, 384));
    // An imported 0% assignment shows `[0%]` and survives an unchanged commit.
    ed.proj.assignments[0].units = 0.0;
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    ed.set_resources(10, &["Bob[0%]".into(), "Cement".into()])
        .unwrap();
    assert_eq!(ed.project(), &before);
    assert_eq!(ed.undo_depth(), depth);
}

#[test]
fn malformed_units_are_rejected_atomically_unless_a_resource_has_that_name() {
    let mut ed = with_bob(ResourceType::Work, 0.5);
    ed.set_resources(10, &["Bob".into()]).unwrap();
    ed.mark_saved();
    let before = ed.project().clone();
    for token in [
        "Bob[abc%]",
        "Bob[-5%]",
        "Bob[0%]",
        "Bob[50]",
        "Bob[%]",
        "Bob[NaN%]",
        "Bob[inf%]",
        "[50%]",
    ] {
        let err = format!("Invalid units in '{token}'");
        assert_eq!(
            ed.set_resources(10, &["Carol".into(), token.into()]),
            Err(err.clone())
        );
        assert_eq!(ed.set_resources(20, &[token.into()]), Err(err.clone()));
        assert_eq!(ed.assign_resource(20, token), Err(err));
        unchanged(&ed, &before, (1, 0, false));
    }
    // A whole-token name match wins over the units suffix.
    for (uid, name) in [(5, "Crew [A%]"), (6, "Rig[50%]")] {
        ed.proj.resources.push(Resource {
            uid,
            id: uid,
            name: name.into(),
            max_units: 1.0,
            ..Resource::default()
        });
    }
    let count = ed.proj.resources.len();
    ed.set_resources(20, &[" crew [a%] ".into(), "RIG[50%]".into()])
        .unwrap();
    assert_eq!(allocation(&ed, 20, 5), (1.0, 480));
    assert_eq!(allocation(&ed, 20, 6), (1.0, 480));
    assert_eq!(
        ed.assign_resource(30, "Rig[50%]").unwrap(),
        AssignOutcome::Assigned
    );
    assert_eq!(allocation(&ed, 30, 6), (1.0, 0));
    assert_eq!(ed.proj.resources.len(), count);
}

#[test]
fn assign_prompt_takes_units_and_changes_only_different_ones() {
    let mut ed = with_bob(ResourceType::Work, 0.5);
    assert_eq!(
        ed.assign_resource(10, "Bob[25%]").unwrap(),
        AssignOutcome::Assigned
    );
    assert_eq!(allocation(&ed, 10, 1), (0.25, 240));
    ed.mark_saved();
    let before = ed.project().clone();
    for same in ["bob", " BOB [25%] "] {
        assert_eq!(
            ed.assign_resource(10, same).unwrap(),
            AssignOutcome::AlreadyAssigned
        );
        unchanged(&ed, &before, (1, 0, false));
    }
    assert_eq!(
        ed.assign_resource(10, "Bob[75%]").unwrap(),
        AssignOutcome::Assigned
    );
    assert_eq!(allocation(&ed, 10, 1), (0.75, 720));
    assert_eq!(ed.proj.assignments.len(), 1);
    assert_eq!(ed.undo_depth(), 2);
    ed.undo();
    assert_eq!(ed.project(), &before);
    ed.assign_resource(20, "Dan[40%]").unwrap();
    assert_eq!(allocation(&ed, 20, resource_uid(&ed, "Dan")), (0.4, 192));
}

#[test]
fn partial_units_survive_mspdi() {
    let mut ed = with_bob(ResourceType::Work, 0.5);
    ed.assign_resource(10, "Bob").unwrap();
    let back = crate::mspdi::read_mspdi(&crate::mspdi::write_mspdi(ed.project())).unwrap();
    let a = back.assignments.iter().find(|a| a.task_uid == 10).unwrap();
    assert_eq!((a.units, a.work_min), (0.5, 480));
    assert_eq!(format_resource_names(&back, 10), "Bob[50%]");
}

#[test]
fn leveling_books_assignments_at_their_units() {
    let mut ed = with_bob(ResourceType::Work, 1.0);
    ed.proj.tasks[1].duration_min = 960;
    ed.set_resources(10, &["Bob[50%]".into()]).unwrap();
    ed.set_resources(20, &["Bob[50%]".into()]).unwrap();
    let starts = |ed: &Editor| {
        let leveled = crate::schedule::level(ed.project());
        [10, 20].map(|uid| leveled.start(uid).unwrap().to_mspdi())
    };
    // Two half-time bookings fit Bob's 100% side by side.
    assert_eq!(starts(&ed), ["2026-01-05T08:00:00", "2026-01-05T08:00:00"]);
    // At 100% the second task must wait for the first.
    ed.set_resources(20, &["Bob".into()]).unwrap();
    assert_eq!(starts(&ed), ["2026-01-05T08:00:00", "2026-01-07T08:00:00"]);
}

#[test]
fn a_literal_bracketed_resource_does_not_steal_the_assigned_cell_text() {
    let mut ed = with_bob(ResourceType::Work, 0.5);
    ed.set_resources(10, &["Bob".into()]).unwrap();
    // Older builds created resources like this when `Bob[50%]` was typed.
    ed.proj.resources.push(Resource {
        uid: 7,
        id: 7,
        name: "Bob[50%]".into(),
        max_units: 1.0,
        ..Resource::default()
    });
    ed.mark_saved();
    let before = ed.project().clone();
    ed.set_resources(10, &["bob[50%] ".into()]).unwrap();
    unchanged(&ed, &before, (1, 0, false));
    ed.set_resources(10, &["Bob[50%]".into(), "Carol".into()])
        .unwrap();
    assert_eq!(allocation(&ed, 10, 1), (0.5, 480));
    assert_eq!(format_resource_names(&ed.proj, 10), "Bob[50%], Carol");
    // Where Bob is not assigned, the whole name still wins, and recommitting keeps it.
    ed.set_resources(20, &["Bob[50%]".into()]).unwrap();
    assert_eq!(allocation(&ed, 20, 7), (1.0, 480));
    let depth = ed.undo_depth();
    ed.set_resources(20, &["bob[50%]".into()]).unwrap();
    assert_eq!(ed.undo_depth(), depth);
    // Both assigned: the cell reads the same text twice, so either token is ambiguous.
    ed.set_resources(10, &["Bob[50%]".into(), "Carol".into(), "Rig".into()])
        .unwrap();
    ed.proj.assignments.push(Assignment {
        uid: 99,
        task_uid: 10,
        resource_uid: 7,
        units: 1.0,
        work_min: 960,
        ..Assignment::default()
    });
    assert_eq!(
        format_resource_names(&ed.proj, 10),
        "Bob[50%], Carol, Rig, Bob[50%]"
    );
    ed.mark_saved();
    let before = ed.project().clone();
    let history = (ed.undo_depth(), ed.redo_depth(), ed.dirty());
    for tokens in [&["Bob[50%]"][..], &["Bob[50%]", "Carol", "Rig", "Bob[50%]"]] {
        let tokens: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        assert_eq!(
            ed.set_resources(10, &tokens),
            Err("Resource name 'Bob[50%]' is ambiguous".into())
        );
        unchanged(&ed, &before, history);
    }
}

/// Two-day task 10 with the given Work resources (all at Max. Units 100%) and assignments.
fn on_task_10(resources: &[(i32, &str)], assigned: &[(i32, f64)]) -> Editor {
    let mut ed = editor();
    ed.proj.tasks[0].duration_min = 960;
    for &(uid, name) in resources {
        ed.proj.resources.push(Resource {
            uid,
            id: uid,
            name: name.into(),
            max_units: 1.0,
            ..Resource::default()
        });
    }
    for (k, &(resource_uid, units)) in assigned.iter().enumerate() {
        ed.proj.assignments.push(Assignment {
            uid: k as i32 + 1,
            task_uid: 10,
            resource_uid,
            units,
            work_min: work_for(960, units),
            ..Assignment::default()
        });
    }
    Editor::new(ed.proj)
}

fn commit_shown_text(ed: &mut Editor) -> Result<(), String> {
    let text = format_resource_names(&ed.proj, 10);
    // The suite splits on commas without trimming, as here.
    let tokens: Vec<String> = text.split(',').map(str::to_owned).collect();
    ed.set_resources(10, &tokens)
}

#[test]
fn names_and_shown_text_share_one_exactness_ladder() {
    // Deleting ALICE's bracket, or changing it, never unassigns ALICE via Alice.
    for (token, units) in [
        (" ALICE", 1.0),
        ("ALICE", 1.0),
        (" ALICE[75%]", 0.75),
        ("ALICE[75%]", 0.75),
    ] {
        let mut ed = on_task_10(&[(1, "Alice"), (2, "ALICE")], &[(1, 1.0), (2, 0.5)]);
        assert_eq!(format_resource_names(&ed.proj, 10), "Alice, ALICE[50%]");
        ed.set_resources(10, &["Alice".into(), token.into()])
            .unwrap();
        assert_eq!(allocation(&ed, 10, 1), (1.0, 960), "{token}");
        assert_eq!(
            allocation(&ed, 10, 2),
            (units, work_for(960, units)),
            "{token}"
        );
    }
    // An exact rendering beats a case-insensitive literal name.
    let mut ed = on_task_10(&[(1, "Bob"), (2, "bob[50%]")], &[(1, 0.5), (2, 1.0)]);
    assert_eq!(format_resource_names(&ed.proj, 10), "Bob[50%], bob[50%]");
    let before = ed.project().clone();
    commit_shown_text(&mut ed).unwrap();
    unchanged(&ed, &before, (0, 0, false));
}

#[test]
fn duplicate_assignments_of_one_resource_survive_an_unchanged_commit() {
    for units in [[1.0, 0.5], [0.5, 1.0], [0.5, 0.5]] {
        let mut ed = on_task_10(&[(1, "Bob")], &[(1, units[0]), (1, units[1])]);
        let before = ed.project().clone();
        commit_shown_text(&mut ed).unwrap();
        unchanged(&ed, &before, (0, 0, false));
    }
}

#[test]
fn units_edits_keep_imported_assignment_fields_but_clear_regular_work() {
    for prompt in [false, true] {
        let mut ed = editor();
        ed.set_resources(10, &["Alice".into()]).unwrap();
        let a = &mut ed.proj.assignments[0];
        a.work_contour = Some(3);
        a.fixed_material = Some(false);
        a.has_fixed_rate_units = Some(true);
        a.start = Some(DateTime::from_ymd_hm(2026, 3, 3, 8, 0));
        a.finish = Some(DateTime::from_ymd_hm(2026, 3, 3, 17, 0));
        a.regular_work_min = Some(480);
        let imported = a.clone();
        if prompt {
            ed.assign_resource(10, "Alice[50%]").unwrap();
        } else {
            ed.set_resources(10, &["Alice[50%]".into()]).unwrap();
        }
        // Regular work was the old Work less overtime; kept, it would invent overtime.
        let a = &ed.proj.assignments[0];
        assert_eq!((a.units, a.work_min, a.regular_work_min), (0.5, 240, None));
        assert_eq!(
            Assignment {
                units: imported.units,
                work_min: imported.work_min,
                regular_work_min: imported.regular_work_min,
                ..a.clone()
            },
            imported,
            "prompt: {prompt}"
        );
    }
}

#[test]
fn new_resources_and_assignments_write_none_of_the_imported_fields() {
    let mut ed = editor();
    ed.set_resources(10, &["Alice".into()]).unwrap();
    ed.assign_resource(20, "Bob").unwrap();
    assert_eq!(ed.proj.resources.len(), 2);
    assert_eq!(ed.proj.assignments.len(), 2);
    let xml = crate::mspdi::write_mspdi(ed.project());
    let section = |name: &str| {
        let open = xml.find(&format!("<{name}>")).unwrap();
        &xml[open..xml.find(&format!("</{name}>")).unwrap()]
    };
    for name in [
        "WorkGroup",
        "PeakUnits",
        "OverAllocated",
        "CanLevel",
        "Work",
        "RegularWork",
        "RemainingWork",
        "StandardRateFormat",
        "OvertimeRateFormat",
        "IsGeneric",
        "IsInactive",
        "BookingType",
        "IsBudget",
    ] {
        assert!(
            !section("Resources").contains(&format!("<{name}>")),
            "{name}"
        );
    }
    for name in [
        "Finish",
        "HasFixedRateUnits",
        "FixedMaterial",
        "RegularWork",
        "Start",
        "WorkContour",
    ] {
        assert!(
            !section("Assignments").contains(&format!("<{name}>")),
            "{name}"
        );
    }
}

#[test]
fn typed_dates_resolve_derived_calendars_like_the_scheduler() {
    // Two calendars share UID 1, the second with a 07:00-19:00 Monday; a task
    // on calendar 2, derived from 1 with Tuesday off.
    let mut ed = editor();
    let mut long = Calendar::standard(1);
    long.week[1] = Some(crate::model::DayWorking {
        times: vec![crate::model::WorkingTime {
            from: 7 * 60,
            to: 19 * 60,
        }],
    });
    let derived = Calendar {
        base_calendar_uid: Some(1),
        week: [
            None,
            None,
            Some(crate::model::DayWorking::default()),
            None,
            None,
            None,
            None,
        ],
        ..Calendar::standard(2)
    };
    ed.proj.calendars = vec![Calendar::standard(1), long, derived];
    ed.proj.tasks[0].calendar_uid = Some(2);
    let task = ed.project().task(10).unwrap().clone();
    let monday = parse_cell_date("2026-03-02").unwrap();
    // The scheduler's rule: the last calendar with UID 1 is the base.
    assert_eq!(
        day_finish(ed.project(), &task, monday)
            .unwrap()
            .minute_of_day(),
        19 * 60
    );
    assert_eq!(
        day_start(ed.project(), &task, monday).minute_of_day(),
        7 * 60
    );
    let tuesday = parse_cell_date("2026-03-03").unwrap();
    assert!(day_finish(ed.project(), &task, tuesday).is_err());
    let mut proj = ed.project().clone();
    proj.start_date = Some(monday.add_minutes(7 * 60));
    proj.tasks[0].duration_min = 12 * 60;
    let sched = crate::schedule::schedule(&proj);
    assert_eq!(
        sched.get(10).unwrap().early_finish,
        day_finish(ed.project(), &task, monday).unwrap()
    );
}
