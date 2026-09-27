use super::*;

const DAY: i64 = 480;

fn resource(uid: i32, name: &str, kind: ResourceType) -> Resource {
    Resource {
        uid,
        id: uid,
        name: name.into(),
        kind,
        max_units: 1.0,
        ..Resource::default()
    }
}

fn assignment(uid: i32, resource_uid: i32, units: f64, work_min: i64) -> Assignment {
    Assignment {
        uid,
        task_uid: 1,
        resource_uid,
        units,
        work_min,
        ..Assignment::default()
    }
}

/// Task 1 of `duration`, typed `kind`, with `assignments`; Bob (1) and Carol
/// (2) are work resources, Cement (3) a material and Fee (4) a cost.
fn plan(
    kind: Option<TaskType>,
    effort_driven: Option<bool>,
    duration: i64,
    assignments: Vec<Assignment>,
) -> Editor {
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![Task {
            uid: 1,
            id: 1,
            name: "Task".into(),
            outline_level: 1,
            duration_min: duration,
            task_type: kind,
            effort_driven,
            ..Task::default()
        }],
        resources: vec![
            resource(1, "Bob", ResourceType::Work),
            resource(2, "Carol", ResourceType::Work),
            resource(3, "Cement", ResourceType::Material),
            resource(4, "Fee", ResourceType::Cost),
        ],
        assignments,
        ..Project::default()
    })
}

/// `(units, work)` of task 1's assignment of resource `rid`.
fn alloc(ed: &Editor, rid: i32) -> (f64, i64) {
    let a = ed
        .proj
        .assignments
        .iter()
        .find(|a| a.task_uid == 1 && a.resource_uid == rid)
        .unwrap();
    (a.units, a.work_min)
}

fn duration(ed: &Editor) -> i64 {
    ed.proj.tasks[0].duration_min
}

fn set(ed: &mut Editor, names: &[&str]) {
    let names: Vec<String> = names.iter().map(|&n| n.to_owned()).collect();
    ed.set_resources(1, &names).unwrap();
}

// ---- a duration edit (AC5) ----

#[test]
fn a_fixed_work_duration_edit_keeps_work_and_changes_units() {
    let mut delayed = assignment(2, 2, 1.0, DAY);
    delayed.delay = Some(DAY * 10); // tenths of a minute: one day
    let mut ed = plan(
        Some(TaskType::FixedWork),
        None,
        2 * DAY,
        vec![
            assignment(1, 1, 1.0, 2 * DAY),
            delayed,
            assignment(3, 3, 5.0, 300),
            assignment(4, 4, 1.0, 0),
        ],
    );
    ed.set_duration(1, "4d").unwrap();
    assert_eq!(duration(&ed), 4 * DAY);
    assert_eq!(alloc(&ed, 1), (0.5, 2 * DAY));
    // Works from its one-day delay: one day of work over three.
    let (units, work) = alloc(&ed, 2);
    assert!((units - 1. / 3.).abs() < 1e-12, "{units}");
    assert_eq!(work, DAY);
    // Material and cost are not time.
    assert_eq!((alloc(&ed, 3), alloc(&ed, 4)), ((5.0, 300), (1.0, 0)));
    assert_eq!(ed.undo_depth(), 1);
    // Nothing left to work in: units stay.
    ed.set_duration(1, "1d").unwrap();
    assert_eq!(alloc(&ed, 2).0, units);
    assert!(ed.undo() && ed.undo());
    assert_eq!(alloc(&ed, 1), (1.0, 2 * DAY));
}

#[test]
fn a_manual_fixed_work_task_s_typed_finish_changes_its_units() {
    let mut ed = plan(
        Some(TaskType::FixedWork),
        None,
        DAY,
        vec![assignment(1, 1, 1.0, DAY)],
    );
    ed.set_manual(1, true).unwrap();
    ed.set_finish(1, DateTime::from_ymd_hm(2026, 1, 6, 0, 0))
        .unwrap();
    assert_eq!(duration(&ed), 2 * DAY);
    assert_eq!(alloc(&ed, 1), (0.5, DAY));
}

#[test]
fn other_task_types_keep_rescaling_work_on_a_duration_edit() {
    for kind in [
        None,
        Some(TaskType::FixedUnits),
        Some(TaskType::FixedDuration),
    ] {
        let mut ed = plan(kind, None, DAY, vec![assignment(1, 1, 0.5, DAY / 2)]);
        ed.set_duration(1, "2d").unwrap();
        assert_eq!(alloc(&ed, 1), (0.5, DAY), "{kind:?}");
    }
}

// ---- a units edit (AC6) ----

#[test]
fn a_units_edit_keeps_work_and_changes_the_duration_unless_fixed_duration() {
    for kind in [None, Some(TaskType::FixedUnits), Some(TaskType::FixedWork)] {
        let mut ed = plan(kind, None, 2 * DAY, vec![assignment(1, 1, 1.0, 2 * DAY)]);
        set(&mut ed, &["Bob[200%]"]);
        assert_eq!(alloc(&ed, 1), (2.0, 2 * DAY), "{kind:?}");
        assert_eq!(duration(&ed), DAY, "{kind:?}");
        assert_eq!(ed.undo_depth(), 1);
        // The Assign prompt does the same.
        ed.assign_resource(1, "Bob[50%]").unwrap();
        assert_eq!((alloc(&ed, 1), duration(&ed)), ((0.5, 2 * DAY), 4 * DAY));
        ed.undo();
        ed.undo();
        assert_eq!((alloc(&ed, 1), duration(&ed)), ((1.0, 2 * DAY), 2 * DAY));
    }
    let mut ed = plan(
        Some(TaskType::FixedDuration),
        None,
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY)],
    );
    set(&mut ed, &["Bob[200%]"]);
    assert_eq!((alloc(&ed, 1), duration(&ed)), ((2.0, 4 * DAY), 2 * DAY));
}

#[test]
fn a_units_edit_stretches_the_task_to_its_longest_assignment() {
    let mut delayed = assignment(2, 2, 1.0, DAY);
    delayed.delay = Some(DAY * 10);
    let mut ed = plan(
        None,
        None,
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY), delayed],
    );
    // Bob finishes sooner; Carol, from her delay, still ends with the task.
    set(&mut ed, &["Bob[200%]", "Carol"]);
    assert_eq!(duration(&ed), 2 * DAY);
    assert_eq!((alloc(&ed, 1), alloc(&ed, 2)), ((2.0, 2 * DAY), (1.0, DAY)));
    // At half her units Carol needs two days after her delay.
    set(&mut ed, &["Bob[200%]", "Carol[50%]"]);
    assert_eq!(duration(&ed), 3 * DAY);
    assert_eq!(alloc(&ed, 2), (0.5, DAY));
}

#[test]
fn a_units_edit_moves_a_manual_task_s_finish() {
    let mut ed = plan(None, None, 2 * DAY, vec![assignment(1, 1, 1.0, 2 * DAY)]);
    ed.set_manual(1, true).unwrap();
    let start = ed.proj.tasks[0].manual_start;
    set(&mut ed, &["Bob[50%]"]);
    let t = &ed.proj.tasks[0];
    assert_eq!(
        (t.duration_min, t.manual_duration_min, t.manual_start),
        (4 * DAY, Some(4 * DAY), start)
    );
    assert_eq!(ed.disp_duration_min(1), Some(4 * DAY));
    assert_eq!(
        ed.proj.tasks[0].stored_finish,
        ed.disp_finish(1),
        "the finish a save writes follows"
    );
}

#[test]
fn a_units_edit_past_the_scheduling_range_changes_nothing() {
    let mut ed = plan(None, None, 2 * DAY, vec![assignment(1, 1, 1.0, 2 * DAY)]);
    let before = ed.project().clone();
    let names = vec!["Bob[1e-300%]".to_owned()];
    assert!(ed.set_resources(1, &names).is_err());
    assert_eq!((ed.project(), ed.undo_depth()), (&before, 0));
}

// ---- adding and removing resources (AC7) ----

#[test]
fn an_effort_driven_task_shares_its_work_with_an_added_resource() {
    for (kind, driven) in [
        (None, Some(true)),
        (Some(TaskType::FixedUnits), Some(true)),
        // Fixed Work is always effort-driven.
        (Some(TaskType::FixedWork), None),
    ] {
        let mut ed = plan(kind, driven, 2 * DAY, vec![assignment(1, 1, 1.0, 2 * DAY)]);
        set(&mut ed, &["Bob", "Carol"]);
        assert_eq!((alloc(&ed, 1), alloc(&ed, 2)), ((1.0, DAY), (1.0, DAY)));
        assert_eq!(duration(&ed), DAY, "{kind:?}");
        assert_eq!(ed.undo_depth(), 1);
        // Removing her gives Bob the work back.
        set(&mut ed, &["Bob"]);
        assert_eq!((alloc(&ed, 1), duration(&ed)), ((1.0, 2 * DAY), 2 * DAY));
        // The Assign prompt adds as the cell does.
        ed.assign_resource(1, "Carol[300%]").unwrap();
        assert_eq!(
            (alloc(&ed, 1), alloc(&ed, 2)),
            ((1.0, DAY / 2), (3.0, 3 * DAY / 2))
        );
        assert_eq!(duration(&ed), DAY / 2);
    }
}

#[test]
fn an_effort_driven_fixed_duration_task_lowers_the_units_instead() {
    let mut ed = plan(
        Some(TaskType::FixedDuration),
        Some(true),
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY)],
    );
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!(duration(&ed), 2 * DAY);
    assert_eq!((alloc(&ed, 1), alloc(&ed, 2)), ((0.5, DAY), (0.5, DAY)));
}

#[test]
fn a_task_that_is_not_effort_driven_adds_work() {
    for driven in [None, Some(false)] {
        let mut ed = plan(None, driven, 2 * DAY, vec![assignment(1, 1, 1.0, 2 * DAY)]);
        set(&mut ed, &["Bob", "Carol"]);
        assert_eq!(duration(&ed), 2 * DAY);
        assert_eq!(
            (alloc(&ed, 1), alloc(&ed, 2)),
            ((1.0, 2 * DAY), (1.0, 2 * DAY))
        );
    }
}

#[test]
fn the_first_assignment_is_not_effort_driven() {
    let mut ed = plan(None, Some(true), 2 * DAY, vec![]);
    set(&mut ed, &["Bob[50%]"]);
    assert_eq!((alloc(&ed, 1), duration(&ed)), ((0.5, DAY), 2 * DAY));
    // Nor is a task whose assignments had no work.
    let mut ed = plan(None, Some(true), 2 * DAY, vec![assignment(1, 1, 1.0, 0)]);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!((alloc(&ed, 2), duration(&ed)), ((1.0, 2 * DAY), 2 * DAY));
}

#[test]
fn adding_resources_and_changing_units_in_one_commit_shares_by_the_new_units() {
    let mut ed = plan(
        None,
        Some(true),
        2 * DAY,
        vec![assignment(1, 1, 1.0, 3 * DAY)],
    );
    set(&mut ed, &["Bob[50%]", "Carol"]);
    // Three days of work at 150%: two days, one third for Bob.
    assert_eq!((alloc(&ed, 1), alloc(&ed, 2)), ((0.5, DAY), (1.0, 2 * DAY)));
    assert_eq!(duration(&ed), 2 * DAY);
    assert_eq!(ed.undo_depth(), 1);
}

#[test]
fn material_and_cost_resources_take_no_part_in_effort() {
    let mut ed = plan(
        None,
        Some(true),
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY)],
    );
    // Only non-work resources added: the work resources are the same.
    set(&mut ed, &["Bob", "Cement[4]", "Fee"]);
    assert_eq!(duration(&ed), 2 * DAY);
    assert_eq!(
        (alloc(&ed, 1), alloc(&ed, 3), alloc(&ed, 4)),
        ((1.0, 2 * DAY), (4.0, 240), (1.0, 0))
    );
    // Adding Carol shares Bob's work only; Cement and Fee keep theirs.
    set(&mut ed, &["Bob", "Carol", "Cement[4]", "Fee"]);
    assert_eq!((alloc(&ed, 1), alloc(&ed, 2)), ((1.0, DAY), (1.0, DAY)));
    assert_eq!((alloc(&ed, 3), alloc(&ed, 4)), ((4.0, 240), (1.0, 0)));
    assert_eq!(duration(&ed), DAY);
}

#[test]
fn contoured_or_delayed_assignments_keep_the_plain_rule() {
    let mut contoured = assignment(1, 1, 1.0, 2 * DAY);
    contoured.work_contour = Some(1);
    let mut ed = plan(None, Some(true), 2 * DAY, vec![contoured.clone()]);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!((alloc(&ed, 2), duration(&ed)), ((1.0, 2 * DAY), 2 * DAY));
    // A units edit on a contoured task rescales work as before too.
    let mut ed = plan(None, None, 2 * DAY, vec![contoured]);
    set(&mut ed, &["Bob[50%]"]);
    assert_eq!((alloc(&ed, 1), duration(&ed)), ((0.5, DAY), 2 * DAY));
    // A delay has no share of the work to redistribute over.
    let mut delayed = assignment(1, 1, 1.0, DAY);
    delayed.delay = Some(DAY * 10);
    let mut ed = plan(None, Some(true), 2 * DAY, vec![delayed]);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!((alloc(&ed, 2), duration(&ed)), ((1.0, 2 * DAY), 2 * DAY));
}

#[test]
fn summaries_and_milestones_keep_the_plain_rule() {
    let mut ed = plan(None, Some(true), 0, vec![assignment(1, 1, 1.0, DAY)]);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!(
        (alloc(&ed, 1), alloc(&ed, 2), duration(&ed)),
        ((1.0, DAY), (1.0, 0), 0)
    );
    let mut proj = plan(
        None,
        Some(true),
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY)],
    )
    .project()
    .clone();
    proj.tasks.push(Task {
        uid: 2,
        id: 2,
        name: "Child".into(),
        outline_level: 2,
        duration_min: DAY,
        ..Task::default()
    });
    let mut ed = Editor::new(proj);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!(
        (alloc(&ed, 1), alloc(&ed, 2)),
        ((1.0, 2 * DAY), (1.0, 2 * DAY))
    );
}

#[test]
fn a_recalculation_leaves_the_estimate_alone() {
    let mut proj = plan(
        None,
        Some(true),
        2 * DAY,
        vec![assignment(1, 1, 1.0, 2 * DAY)],
    )
    .project()
    .clone();
    proj.tasks[0].estimated = Some(true);
    let mut ed = Editor::new(proj);
    set(&mut ed, &["Bob", "Carol"]);
    assert_eq!(duration(&ed), DAY);
    assert_eq!(ed.proj.tasks[0].estimated, Some(true));
}
