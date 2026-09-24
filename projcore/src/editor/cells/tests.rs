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
    for day in &mut ed.proj.calendars[0].week {
        day.times = vec![crate::model::WorkingTime { from: 0, to: 1440 }];
    }
    // Keep a resolved leaf link so this project needs a backward horizon.
    ed.set_predecessors(
        20,
        vec![Predecessor {
            uid: 30,
            link: LinkType::FinishStart,
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
        ("Alice ", "Alice "),
        (" Alice", " alice"),
        (" Alice ", " ALICE "),
    ] {
        let mut ed = editor();
        ed.assign_resource(10, "Alice").unwrap();
        ed.proj.resources[0].name = stored.into();
        ed.proj.assignments[0].units = 0.5;
        ed.proj.assignments[0].work_min = 123;
        let retained = ed.proj.assignments[0];
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
    let retained = ed.proj.assignments[1];
    let before = ed.project().clone();
    let depth = ed.undo_depth();
    ed.set_resources(10, &["Alice".into(), "Bob".into()])
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
    // One assigned match wins even for a name-only no-op, preserving redo.
    ed.set_resources(10, &["alice".into()]).unwrap();
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
