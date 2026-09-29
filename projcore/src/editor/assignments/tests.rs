use super::*;
use crate::model::{Rate, RateEntry};

const DAY: i64 = 480;

fn resource(uid: i32, name: &str, kind: ResourceType, rate: &str) -> Resource {
    Resource {
        uid,
        id: uid,
        name: name.into(),
        kind,
        max_units: 1.0,
        standard_rate: Rate::parse(rate),
        ..Resource::default()
    }
}

/// Task 1 (5 days, typed `kind`) with `assignments` and a blank row 2. Ann
/// (1) is $50/h, $80/h on table B; Bob (2) $30/h; Steel (3) a material at
/// $10 a unit; Fee (4) a cost resource.
fn plan(kind: Option<TaskType>, effort_driven: bool, assignments: Vec<Assignment>) -> Editor {
    let mut ann = resource(1, "Ann", ResourceType::Work, "50");
    ann.rates = vec![RateEntry {
        rate_table: Some(1),
        standard_rate: Rate::parse("80"),
        ..RateEntry::default()
    }];
    Editor::new(Project {
        start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
        tasks: vec![
            Task {
                uid: 1,
                id: 1,
                name: "Build".into(),
                outline_level: 1,
                duration_min: 5 * DAY,
                task_type: kind,
                effort_driven: Some(effort_driven),
                // The refresh moves stored totals; it never fills absent ones.
                work_min: Some(0),
                cost: Rate::parse("0"),
                ..Task::default()
            },
            Task {
                uid: 2,
                id: 2,
                is_null: true,
                ..Task::default()
            },
        ],
        resources: vec![
            ann,
            resource(2, "Bob", ResourceType::Work, "30"),
            resource(3, "Steel", ResourceType::Material, "10"),
            resource(4, "Fee", ResourceType::Cost, "0"),
        ],
        assignments,
        ..Project::default()
    })
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

fn get(ed: &Editor, uid: i32) -> &Assignment {
    ed.proj.assignments.iter().find(|a| a.uid == uid).unwrap()
}

fn cost(ed: &Editor, uid: i32) -> f64 {
    get(ed, uid).cost.as_ref().and_then(Rate::to_f64).unwrap() / 100.0
}

fn duration(ed: &Editor) -> i64 {
    ed.proj.tasks[0].duration_min
}

/// A rejected edit leaves the plan, history, redo and dirty flag as they were.
fn assert_untouched(ed: &mut Editor, edit: impl FnOnce(&mut Editor) -> Result<(), String>) {
    let before = (ed.proj.clone(), ed.undo.clone(), ed.redo.clone(), ed.dirty);
    assert!(edit(ed).is_err());
    assert_eq!(
        (ed.proj.clone(), ed.undo.clone(), ed.redo.clone(), ed.dirty),
        before
    );
}

/// An edit is one undo step, and undo restores the plan exactly.
fn assert_one_step<T>(ed: &mut Editor, edit: impl FnOnce(&mut Editor) -> T) -> T {
    let before = ed.proj.clone();
    let depth = ed.undo_depth();
    let out = edit(ed);
    assert_eq!(ed.undo_depth(), depth + 1);
    assert!(ed.dirty());
    assert!(ed.undo());
    assert_eq!(ed.proj, before);
    assert!(ed.redo());
    out
}

fn units(u: f64) -> AssignmentPatch {
    AssignmentPatch {
        units: Some(u),
        ..AssignmentPatch::default()
    }
}

fn work(min: i64) -> AssignmentPatch {
    AssignmentPatch {
        work_min: Some(min),
        ..AssignmentPatch::default()
    }
}

fn table(t: u8) -> AssignmentPatch {
    AssignmentPatch {
        rate_table: Some(t),
        ..AssignmentPatch::default()
    }
}

fn delay(min: i64) -> AssignmentPatch {
    AssignmentPatch {
        delay_min: Some(min),
        ..AssignmentPatch::default()
    }
}

#[test]
fn add_set_and_delete_round_trip_one_undo_step_each() {
    let mut ed = plan(Some(TaskType::FixedDuration), false, vec![]);
    let ann = assert_one_step(&mut ed, |ed| {
        ed.add_assignment(1, ResourceRef::Uid(1), Some(1.0), None)
            .unwrap()
    });
    let bob = assert_one_step(&mut ed, |ed| {
        ed.add_assignment(1, ResourceRef::Name("bob"), Some(0.5), None)
            .unwrap()
    });
    assert_eq!((get(&ed, ann).work_min, cost(&ed, ann)), (5 * DAY, 2000.0));
    assert_eq!(get(&ed, bob).resource_uid, 2, "names match ignoring case");
    assert_eq!((get(&ed, bob).work_min, cost(&ed, bob)), (20 * 60, 600.0));
    // Fixed Duration: the work follows the units.
    assert_one_step(&mut ed, |ed| ed.set_assignment(bob, units(1.0)).unwrap());
    assert_eq!((get(&ed, bob).work_min, cost(&ed, bob)), (40 * 60, 1200.0));
    assert_eq!(duration(&ed), 5 * DAY);
    assert_eq!(ed.proj.tasks[0].work_min, Some(80 * 60));
    let task_cost = ed.proj.tasks[0].cost.as_ref().and_then(Rate::to_f64);
    assert_eq!(task_cost, Some(320000.0));
    let ann_before = get(&ed, ann).clone();
    let task = assert_one_step(&mut ed, |ed| ed.delete_assignment(bob).unwrap());
    assert_eq!(task, 1);
    assert_eq!(get(&ed, ann), &ann_before);
    assert!(ed.proj.assignments.iter().all(|a| a.uid != bob));
    assert_one_step(&mut ed, |ed| ed.set_assignment(ann, table(1)).unwrap());
    assert_eq!(get(&ed, ann).cost_rate_table, Some(1));
    assert_eq!(cost(&ed, ann), 3200.0);
}

#[test]
fn a_units_edit_on_fixed_units_keeps_work_and_stretches_the_task() {
    let mut ed = plan(None, false, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    ed.set_assignment(1, units(0.5)).unwrap();
    assert_eq!((get(&ed, 1).units, get(&ed, 1).work_min), (0.5, 5 * DAY));
    assert_eq!(duration(&ed), 10 * DAY);
}

#[test]
fn units_and_work_together_keep_both_on_fixed_units() {
    let mut ed = plan(None, false, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    let patch = AssignmentPatch {
        units: Some(0.5),
        work_min: Some(2 * DAY),
        ..AssignmentPatch::default()
    };
    assert_one_step(&mut ed, |ed| ed.set_assignment(1, patch).unwrap());
    assert_eq!((get(&ed, 1).units, get(&ed, 1).work_min), (0.5, 2 * DAY));
    assert_eq!(duration(&ed), 4 * DAY);
}

#[test]
fn a_work_edit_clears_overtime() {
    let mut a = assignment(1, 1, 1.0, 5 * DAY);
    a.overtime_work_min = Some(60);
    let mut ed = plan(None, false, vec![a]);
    ed.set_assignment(1, work(4 * DAY)).unwrap();
    assert_eq!(get(&ed, 1).overtime_work_min, None);
    assert_eq!(get(&ed, 1).regular_work_min, Some(4 * DAY));
}

#[test]
fn a_units_edit_works_from_the_delay_on_a_fixed_duration_task() {
    let mut a = assignment(1, 1, 1.0, 3 * DAY);
    a.delay = Some(2 * DAY * 10);
    let mut ed = plan(Some(TaskType::FixedDuration), false, vec![a]);
    ed.set_assignment(1, units(0.5)).unwrap();
    assert_eq!(get(&ed, 1).work_min, 3 * DAY / 2);
    assert_eq!(duration(&ed), 5 * DAY);
}

#[test]
fn a_delay_edit_is_stored_in_tenths() {
    let mut ed = plan(None, false, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    ed.set_assignment(1, delay(DAY)).unwrap();
    assert_eq!(get(&ed, 1).delay, Some(DAY * 10));
    assert_eq!(duration(&ed), 6 * DAY);
}

#[test]
fn values_the_assignment_already_has_record_nothing() {
    let mut ed = plan(None, false, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    ed.rename(1, "Undone").unwrap();
    ed.undo();
    for clean in [false, true] {
        if clean {
            ed.mark_saved();
        }
        for patch in [table(0), units(1.0), work(5 * DAY), delay(0)] {
            let before = (
                ed.proj.clone(),
                ed.undo_depth(),
                ed.redo_depth(),
                ed.dirty(),
            );
            ed.set_assignment(1, patch).unwrap();
            let after = (
                ed.proj.clone(),
                ed.undo_depth(),
                ed.redo_depth(),
                ed.dirty(),
            );
            assert_eq!(after, before);
        }
    }
}

#[test]
fn a_new_resource_name_is_staged_and_one_undo_removes_both() {
    let mut ed = plan(None, false, vec![]);
    let uid = assert_one_step(&mut ed, |ed| {
        ed.add_assignment(1, ResourceRef::Name(" Carol "), None, None)
            .unwrap()
    });
    let carol = ed
        .proj
        .resources
        .iter()
        .find(|r| r.name == "Carol")
        .unwrap();
    assert_eq!((carol.uid, carol.kind), (5, ResourceType::Work));
    assert_eq!(get(&ed, uid).resource_uid, 5);
    assert_eq!(get(&ed, uid).units, 1.0);
}

#[test]
fn adding_to_a_blank_row_makes_it_a_task() {
    let mut ed = plan(None, false, vec![]);
    let uid = assert_one_step(&mut ed, |ed| {
        ed.add_assignment(2, ResourceRef::Uid(2), None, None)
            .unwrap()
    });
    assert!(!ed.proj.tasks[1].is_null);
    assert_eq!(get(&ed, uid).task_uid, 2);
    assert_eq!(get(&ed, uid).work_min, ed.proj.tasks[1].duration_min);
}

#[test]
fn added_work_is_kept_on_an_effort_driven_task() {
    let mut ed = plan(None, true, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    let bob = assert_one_step(&mut ed, |ed| {
        ed.add_assignment(1, ResourceRef::Uid(2), None, Some(2 * DAY))
            .unwrap()
    });
    assert_eq!(get(&ed, bob).work_min, 2 * DAY);
}

#[test]
fn deleting_from_an_effort_driven_task_keeps_its_work() {
    let both = vec![
        assignment(1, 1, 1.0, 5 * DAY),
        assignment(2, 2, 1.0, 5 * DAY),
    ];
    let mut ed = plan(None, true, both);
    assert_one_step(&mut ed, |ed| ed.delete_assignment(2).unwrap());
    assert_eq!(get(&ed, 1).work_min, 10 * DAY);
    assert_eq!(duration(&ed), 10 * DAY);
}

#[test]
fn rejected_edits_leave_the_editor_untouched() {
    let mut steel = assignment(3, 3, 5.0, 300);
    steel.uid = 3;
    let mut fee = assignment(4, 4, 1.0, 0);
    fee.uid = 4;
    let mut ed = plan(
        Some(TaskType::FixedDuration),
        false,
        vec![assignment(1, 1, 1.0, 5 * DAY), steel, fee],
    );
    ed.rename(1, "Undone").unwrap();
    ed.undo();
    let add = |task, r, units, work| {
        move |ed: &mut Editor| ed.add_assignment(task, r, units, work).map(drop)
    };
    for edit in [
        add(1, ResourceRef::Uid(99), None, None),
        add(99, ResourceRef::Uid(2), None, None),
        add(1, ResourceRef::Uid(1), None, None),
        add(1, ResourceRef::Name("  "), None, None),
        add(1, ResourceRef::Uid(2), Some(0.0), None),
        add(1, ResourceRef::Uid(2), Some(-1.0), None),
        add(1, ResourceRef::Uid(2), Some(f64::NAN), None),
        add(1, ResourceRef::Uid(2), Some(f64::INFINITY), None),
        add(1, ResourceRef::Uid(2), None, Some(-60)),
        add(2, ResourceRef::Name("Steel"), None, Some(60)),
    ] {
        assert_untouched(&mut ed, edit);
    }
    let err = ed
        .add_assignment(1, ResourceRef::Uid(1), None, None)
        .unwrap_err();
    assert_eq!(err, "'Ann' is already assigned to task 1; use assign.set");
    let set = |uid, patch: AssignmentPatch| move |ed: &mut Editor| ed.set_assignment(uid, patch);
    for edit in [
        set(99, units(1.0)),
        set(1, AssignmentPatch::default()),
        set(1, units(0.0)),
        set(1, units(f64::NAN)),
        set(1, work(-1)),
        set(1, delay(-1)),
        set(1, table(5)),
        set(3, work(60)),
        set(4, units(2.0)),
        set(4, work(60)),
    ] {
        assert_untouched(&mut ed, edit);
    }
    // Work and delays beyond the scheduling horizon.
    for patch in [work(MAX_MINUTES + 1), delay(MAX_MINUTES + 1)] {
        let err = ed.set_assignment(1, patch.clone()).unwrap_err();
        assert!(err.ends_with("is beyond the scheduling range"), "{err}");
        assert_untouched(&mut ed, |ed| ed.set_assignment(1, patch));
    }
    assert_untouched(&mut ed, |ed| {
        ed.add_assignment(1, ResourceRef::Uid(2), None, Some(MAX_MINUTES + 1))
            .map(drop)
    });
    // A material takes no work, refused before the add is staged: inside a
    // caller's batch, which would keep an add staged first, nothing is added.
    ed.batch(|ed| {
        let err = ed
            .add_assignment(2, ResourceRef::Name("steel"), None, Some(60))
            .unwrap_err();
        assert_eq!(err, "'Steel' is a material resource; set its units");
        assert!(ed.proj.assignments.iter().all(|a| a.task_uid != 2));
        Ok(())
    })
    .unwrap();
    assert!(ed.proj.assignments.iter().all(|a| a.task_uid != 2));
    // Units whose work would pass the scheduling horizon.
    let huge = 1e300;
    let add = |task, rid| {
        move |ed: &mut Editor| {
            ed.add_assignment(task, ResourceRef::Uid(rid), Some(huge), None)
                .map(drop)
        }
    };
    for edit in [add(2, 2), add(2, 3)] {
        assert_untouched(&mut ed, edit);
    }
    for uid in [1, 3] {
        let err = ed.set_assignment(uid, units(huge)).unwrap_err();
        assert_eq!(err, "units are beyond the scheduling range");
        assert_untouched(&mut ed, |ed| ed.set_assignment(uid, units(huge)));
    }
    assert_untouched(&mut ed, |ed| ed.delete_assignment(99).map(drop));
    assert_eq!(
        ed.set_assignment(1, AssignmentPatch::default())
            .unwrap_err(),
        "nothing to set: give units, work, rate_table or delay"
    );
}

#[test]
fn fixed_duration_work_needs_a_span_after_the_delay_the_patch_leaves() {
    // A delay that grows the task leaves room for the work: one patch
    // agrees with the two edits made one after the other.
    let patch = AssignmentPatch {
        delay_min: Some(6 * DAY),
        work_min: Some(8 * 60),
        ..AssignmentPatch::default()
    };
    let mut one = plan(
        Some(TaskType::FixedDuration),
        false,
        vec![assignment(1, 1, 1.0, 5 * DAY)],
    );
    assert_one_step(&mut one, |ed| ed.set_assignment(1, patch).unwrap());
    let mut two = plan(
        Some(TaskType::FixedDuration),
        false,
        vec![assignment(1, 1, 1.0, 5 * DAY)],
    );
    two.set_assignment(1, delay(6 * DAY)).unwrap();
    two.set_assignment(1, work(8 * 60)).unwrap();
    assert_eq!(one.proj, two.proj);
    assert_eq!(duration(&one), 11 * DAY);
    // An assignment without work does not grow the task, so its delay can
    // leave no span for new work.
    let mut ed = plan(
        Some(TaskType::FixedDuration),
        false,
        vec![assignment(1, 1, 1.0, 0)],
    );
    let too_late = AssignmentPatch {
        delay_min: Some(5 * DAY),
        work_min: Some(DAY),
        ..AssignmentPatch::default()
    };
    let err = ed.set_assignment(1, too_late.clone()).unwrap_err();
    assert_eq!(err, "delay must be shorter than the task");
    assert_untouched(&mut ed, |ed| ed.set_assignment(1, too_late));
}

#[test]
fn given_work_survives_a_units_edit_even_when_it_equals_the_stored_work() {
    let both = |u: f64, w: i64| AssignmentPatch {
        units: Some(u),
        work_min: Some(w),
        ..AssignmentPatch::default()
    };
    // Fixed Duration: units given with work are recomputed from the work.
    for (w, units) in [(40 * 60, 1.0), (39 * 60, 0.975), (8 * 60, 0.2)] {
        let mut ed = plan(
            Some(TaskType::FixedDuration),
            false,
            vec![assignment(1, 1, 1.0, 5 * DAY)],
        );
        ed.set_assignment(1, both(0.5, w)).unwrap();
        assert_eq!((get(&ed, 1).units, get(&ed, 1).work_min), (units, w));
        assert_eq!(duration(&ed), 5 * DAY);
    }
    // Fixed Units keeps both and moves the duration.
    let mut ed = plan(None, false, vec![assignment(1, 1, 1.0, 5 * DAY)]);
    ed.set_assignment(1, both(0.5, 5 * DAY)).unwrap();
    assert_eq!((get(&ed, 1).units, get(&ed, 1).work_min), (0.5, 5 * DAY));
    assert_eq!(duration(&ed), 10 * DAY);
}

#[test]
fn a_new_resource_name_with_a_comma_is_refused_an_existing_one_matches() {
    let mut ed = plan(None, false, vec![]);
    assert_untouched(&mut ed, |ed| {
        let err = ed
            .add_assignment(1, ResourceRef::Name("Smith, J"), None, None)
            .unwrap_err();
        assert_eq!(err, "Resource name 'Smith, J' cannot contain a comma");
        Err(err)
    });
    let mut p = ed.proj.clone();
    p.resources
        .push(resource(5, "Smith, J", ResourceType::Work, "40"));
    let mut ed = Editor::new(p);
    let uid = ed
        .add_assignment(1, ResourceRef::Name("smith, j"), None, None)
        .unwrap();
    assert_eq!(get(&ed, uid).resource_uid, 5);
}

#[test]
fn a_new_resource_name_that_is_a_number_is_refused_an_existing_one_matches() {
    let mut ed = plan(None, false, vec![]);
    assert_untouched(&mut ed, |ed| {
        let err = ed
            .add_assignment(1, ResourceRef::Name(" 2 "), None, None)
            .unwrap_err();
        assert_eq!(
            err,
            "no resource named '2'; pass a resource uid as a number"
        );
        Err(err)
    });
    let mut p = ed.proj.clone();
    p.resources.push(resource(5, "2", ResourceType::Work, "40"));
    let mut ed = Editor::new(p);
    let uid = ed
        .add_assignment(1, ResourceRef::Name("2"), None, None)
        .unwrap();
    assert_eq!(get(&ed, uid).resource_uid, 5);
}

#[test]
fn a_material_takes_units_and_a_cost_resource_neither() {
    let mut ed = plan(None, false, vec![]);
    let steel = ed
        .add_assignment(1, ResourceRef::Uid(3), Some(4.0), None)
        .unwrap();
    assert_eq!(get(&ed, steel).work_min, 240);
    ed.set_assignment(steel, units(6.0)).unwrap();
    assert_eq!(get(&ed, steel).work_min, 360);
    let fee = ed
        .add_assignment(1, ResourceRef::Uid(4), None, None)
        .unwrap();
    assert_eq!(get(&ed, fee).work_min, 0);
    assert_eq!(
        ed.add_assignment(2, ResourceRef::Uid(4), Some(1.0), None)
            .unwrap_err(),
        "'Fee' is a cost resource; it takes no units or work"
    );
}
