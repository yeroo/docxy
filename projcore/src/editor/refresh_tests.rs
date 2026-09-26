//! #269: an edit refreshes the assignment, task and resource fields derived
//! from what it changed, and nothing else.
use super::*;
use crate::model::Rate;

fn at(day: u32, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 1, day, hour, 0)
}

fn text(rate: &Option<Rate>) -> Option<&str> {
    rate.as_ref().map(Rate::as_str)
}

fn task(uid: i32, days: i64) -> Task {
    Task {
        uid,
        id: uid,
        name: format!("Task {uid}"),
        outline_level: 1,
        duration_min: days * 480,
        ..Task::default()
    }
}

/// Alice at $50/h and $100 per use.
fn alice() -> Resource {
    Resource {
        uid: 1,
        id: 1,
        name: "Alice".into(),
        max_units: 1.0,
        standard_rate: Rate::parse("50"),
        cost_per_use: Rate::parse("10000"),
        ..Resource::default()
    }
}

/// An assignment as a file carries it, with values that disagree with the
/// plan, so a test sees which ones an edit rewrote.
fn stale(uid: i32, task_uid: i32, units: f64, work_min: i64) -> Assignment {
    Assignment {
        uid,
        task_uid,
        resource_uid: 1,
        units,
        work_min,
        start: Some(at(1, 8)),
        finish: Some(at(1, 9)),
        regular_work_min: Some(1),
        remaining_work_min: Some(1),
        cost: Rate::parse("1"),
        remaining_cost: Rate::parse("1"),
        timephased_data: [1, 2]
            .map(|kind| TimephasedValue {
                kind,
                uid: Some(uid),
                value: Some("PT8H0M0S".into()),
                ..TimephasedValue::default()
            })
            .into(),
        ..Assignment::default()
    }
}

/// Tasks 1 and 2 (one day each, from Monday 5 January), with Alice on both.
fn staffed() -> Editor {
    Editor::new(Project {
        start_date: Some(at(5, 8)),
        tasks: vec![task(1, 1), task(2, 1)],
        resources: vec![alice()],
        assignments: vec![stale(1, 1, 1.0, 480), stale(2, 2, 1.0, 480)],
        ..Project::default()
    })
}

fn assignment(ed: &Editor, uid: i32) -> &Assignment {
    ed.project()
        .assignments
        .iter()
        .find(|a| a.uid == uid)
        .unwrap()
}

#[test]
fn a_duration_edit_refreshes_its_assignments_and_only_those() {
    let mut ed = staffed();
    let untouched = assignment(&ed, 2).clone();
    ed.set_duration_min(1, 960).unwrap();
    let a = assignment(&ed, 1);
    assert_eq!(
        (a.work_min, a.start, a.finish),
        (960, Some(at(5, 8)), Some(at(6, 17)))
    );
    // 16h at $50 plus $100 per use, in hundredths.
    assert_eq!(
        (text(&a.cost), text(&a.remaining_cost)),
        (Some("90000"), Some("90000"))
    );
    assert_eq!(
        (a.regular_work_min, a.remaining_work_min),
        (Some(960), Some(960))
    );
    // The planned spread described the old dates; actual work stays.
    let kinds: Vec<u8> = a.timephased_data.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, [2]);
    // Task 2 did not move: its stale values stay as read.
    assert_eq!(assignment(&ed, 2), &untouched);
}

#[test]
fn a_units_edit_reprices_and_redates_the_assignment() {
    let mut ed = staffed();
    ed.assign_resource(1, "Alice[50%]").unwrap();
    let a = assignment(&ed, 1);
    assert_eq!((a.units, a.work_min), (0.5, 240));
    // 240 minutes at 50% take the whole day.
    assert_eq!((a.start, a.finish), (Some(at(5, 8)), Some(at(5, 17))));
    assert_eq!(text(&a.cost), Some("30000"));
}

#[test]
fn a_successor_moved_by_its_predecessor_is_refreshed() {
    let mut ed = staffed();
    ed.add_predecessor(2, 1, LinkType::FinishStart, 0).unwrap();
    assert_eq!(assignment(&ed, 2).start, Some(at(6, 8)));
    ed.set_duration_min(1, 1440).unwrap();
    let a = assignment(&ed, 2);
    assert_eq!((a.start, a.finish), (Some(at(8, 8)), Some(at(8, 17))));
    assert_eq!(text(&a.cost), Some("50000"));
}

#[test]
fn a_new_assignment_gets_dates_and_a_cost() {
    let mut ed = staffed();
    ed.proj.resources.push(Resource {
        uid: 2,
        id: 2,
        name: "Bob".into(),
        max_units: 1.0,
        standard_rate: Rate::parse("20"),
        ..Resource::default()
    });
    ed.assign_resource(2, "Bob").unwrap();
    let bob = ed.project().assignments.last().unwrap();
    assert_eq!((bob.start, bob.finish), (Some(at(5, 8)), Some(at(5, 17))));
    assert_eq!(text(&bob.cost), Some("16000"));
    assert_eq!(
        (bob.regular_work_min, bob.remaining_work_min),
        (Some(480), Some(480))
    );
}

#[test]
fn assignment_dates_follow_the_schedule_after_stamping() {
    // No project start: the earliest stored or pinned start anchors the
    // plan. Moving manual task 1 moves the anchor only once its new dates
    // are stamped, and unlinked task 2 moves with it.
    let mut first = task(1, 1);
    first.manual = true;
    first.manual_start = Some(at(5, 8));
    first.manual_duration_min = Some(480);
    first.stored_start = Some(at(5, 8));
    first.stored_finish = Some(at(5, 17));
    let mut ed = Editor::new(Project {
        tasks: vec![first, task(2, 1)],
        resources: vec![alice()],
        assignments: vec![stale(2, 2, 1.0, 480)],
        ..Project::default()
    });
    assert_eq!(ed.schedule().get(2).unwrap().early_start, at(5, 8));
    ed.set_start_at(1, at(12, 8)).unwrap();
    let moved = ed.schedule().get(2).unwrap().early_start;
    assert_eq!(moved, at(12, 8));
    let a = assignment(&ed, 2);
    assert_eq!((a.start, a.finish), (Some(moved), Some(at(12, 17))));
}

#[test]
fn resource_and_task_totals_follow_their_assignments() {
    let mut ed = staffed();
    // The file's totals: task 1's cost holds a $25 fixed cost besides its
    // assignment's, and task 2 has none stored.
    ed.proj.tasks[0].work_min = Some(480);
    ed.proj.tasks[0].cost = Rate::parse("2501");
    ed.proj.tasks[0].remaining_work_min = Some(480);
    ed.proj.resources[0].work_min = Some(7);
    ed.proj.resources[0].cost = Rate::parse("7");
    let untouched = ed.proj.tasks[1].clone();
    ed = Editor::new(ed.proj);
    ed.set_duration_min(1, 960).unwrap();
    let t = &ed.project().tasks[0];
    // Each total moves by its assignment's: work 480 → 960, remaining work
    // from the stale 1 to 960, cost from the stale 1 to 90000.
    assert_eq!((t.work_min, t.remaining_work_min), (Some(960), Some(1439)));
    assert_eq!(text(&t.cost), Some("92500"));
    assert_eq!(ed.project().tasks[1], untouched);
    // Alice sums both assignments: task 2's is still as read.
    let r = &ed.project().resources[0];
    assert_eq!(
        (
            r.work_min,
            r.regular_work_min,
            r.overtime_work_min,
            r.remaining_work_min
        ),
        (Some(1440), Some(1440), Some(0), Some(961))
    );
    assert_eq!(
        (text(&r.cost), text(&r.remaining_cost)),
        (Some("90001"), Some("90001"))
    );
    assert_eq!((r.start, r.finish), (Some(at(1, 8)), Some(at(6, 17))));
}

#[test]
fn removing_assignments_lowers_the_resource_totals() {
    let mut ed = staffed();
    ed.set_duration_min(1, 960).unwrap();
    ed.set_duration_min(2, 1440).unwrap();
    let r = &ed.project().resources[0];
    assert_eq!((r.work_min, text(&r.cost)), (Some(2400), Some("220000")));

    ed.assign_resource(2, "").unwrap();
    let r = &ed.project().resources[0];
    assert_eq!((r.work_min, text(&r.cost)), (Some(960), Some("90000")));
    assert_eq!((r.start, r.finish), (Some(at(5, 8)), Some(at(6, 17))));

    ed.delete_task(1).unwrap();
    let r = &ed.project().resources[0];
    assert_eq!((r.work_min, r.remaining_work_min), (Some(0), Some(0)));
    assert_eq!(
        (text(&r.cost), text(&r.remaining_cost)),
        (Some("0"), Some("0"))
    );
    assert_eq!((r.start, r.finish), (None, None));
}

#[test]
fn a_delayed_assignment_still_finishes_with_its_task_after_a_duration_edit() {
    let mut ed = staffed();
    ed.proj.assignments[0].delay = Some(480 * 10);
    ed = Editor::new(ed.proj);
    ed.set_duration_min(1, 3 * 480).unwrap();
    let a = assignment(&ed, 1);
    // A day's delay leaves two days of work in a three-day task.
    assert_eq!(a.work_min, 960);
    assert_eq!((a.start, a.finish), (Some(at(6, 8)), Some(at(7, 17))));
    assert_eq!(a.finish, Some(ed.schedule().get(1).unwrap().early_finish));
}

/// The `<name>` section of a saved plan.
fn section<'a>(xml: &'a str, name: &str) -> &'a str {
    let open = xml.find(&format!("<{name}>")).unwrap();
    &xml[open..xml.find(&format!("</{name}>")).unwrap()]
}

#[test]
fn nothing_is_refreshed_without_an_edit_that_changes_it() {
    // Stored values no rate or schedule would produce survive opening,
    // saving, and an edit that changes none of their inputs.
    let mut proj = staffed().project().clone();
    proj.resources[0].work_min = Some(7);
    proj.resources[0].cost = Rate::parse("7");
    proj.resources[0].start = Some(at(1, 8));
    let xml = crate::mspdi::write_mspdi(&proj);
    let mut ed = Editor::new(crate::mspdi::read_mspdi(&xml).unwrap());
    let saved = crate::mspdi::write_mspdi(ed.project());
    for name in ["Resources", "Assignments"] {
        assert_eq!(section(&saved, name), section(&xml, name), "{name}");
    }
    ed.rename(1, "Renamed").unwrap();
    let saved = crate::mspdi::write_mspdi(ed.project());
    for name in ["Resources", "Assignments"] {
        assert_eq!(section(&saved, name), section(&xml, name), "{name}");
    }

    // Undo and redo restore the models exactly, refreshing nothing.
    let before = ed.project().clone();
    ed.set_duration_min(1, 960).unwrap();
    let after = ed.project().clone();
    assert_ne!(after.assignments, before.assignments);
    assert!(ed.undo());
    assert_eq!(ed.project(), &before);
    assert!(ed.redo());
    assert_eq!(ed.project(), &after);
    // Nor does toggling leveling.
    ed.toggle_level();
    assert_eq!(ed.project(), &after);
}

/// Cement at $5 a unit and $10 per use.
fn cement() -> Resource {
    Resource {
        uid: 3,
        id: 3,
        name: "Cement".into(),
        kind: ResourceType::Material,
        max_units: 1.0,
        standard_rate: Rate::parse("5"),
        cost_per_use: Rate::parse("1000"),
        ..Resource::default()
    }
}

#[test]
fn a_material_is_its_quantity_in_hours_and_priced_per_unit() {
    let mut ed = Editor::new(Project {
        start_date: Some(at(5, 8)),
        tasks: vec![task(1, 5)],
        resources: vec![cement()],
        ..Project::default()
    });
    ed.assign_resource(1, "Cement").unwrap();
    let a = &ed.project().assignments[0];
    // One unit, not five days of it: $5 plus $10 per use.
    assert_eq!((a.units, a.work_min), (1.0, 60));
    assert_eq!(text(&a.cost), Some("1500"));
    ed.assign_resource(1, "Cement[300%]").unwrap();
    let a = &ed.project().assignments[0];
    assert_eq!((a.work_min, text(&a.cost)), (180, Some("2500")));
}

#[test]
fn a_material_moves_its_tasks_cost_but_not_its_work() {
    let mut ed = staffed();
    ed.proj.resources.push(cement());
    ed.proj.tasks[0].work_min = Some(480);
    ed.proj.tasks[0].remaining_work_min = Some(480);
    ed.proj.tasks[0].cost = Rate::parse("2501");
    ed = Editor::new(ed.proj);
    ed.set_resources(1, &["Alice".into(), "Cement".into()])
        .unwrap();
    let t = &ed.project().tasks[0];
    assert_eq!((t.work_min, t.remaining_work_min), (Some(480), Some(480)));
    assert_eq!(text(&t.cost), Some("4001"));
    // The material resource's own Work is its quantity, as in Project.
    assert_eq!(ed.project().resources[1].work_min, Some(60));
    ed.set_resources(1, &["Alice".into()]).unwrap();
    let t = &ed.project().tasks[0];
    assert_eq!((t.work_min, text(&t.cost)), (Some(480), Some("2501")));
    // And none once it is removed.
    assert_eq!(ed.project().resources[1].work_min, Some(0));
}

#[test]
fn summaries_move_with_their_subtasks() {
    // The project summary (UID 0) over summary 3 over tasks 1 and 2.
    let level = |mut t: Task, outline_level: u32, work: i64, cost: &str| {
        t.outline_level = outline_level;
        t.work_min = Some(work);
        t.cost = Rate::parse(cost);
        t
    };
    let mut ed = staffed();
    ed.proj.tasks = vec![
        level(task(0, 0), 0, 960, "3002"),
        level(task(3, 0), 1, 960, "3002"),
        level(task(1, 1), 2, 480, "1501"),
        level(task(2, 1), 2, 480, "1501"),
    ];
    ed = Editor::new(ed.proj);
    let totals = |ed: &Editor| -> Vec<(i32, Option<i64>, Option<String>)> {
        ed.project()
            .tasks
            .iter()
            .map(|t| (t.uid, t.work_min, text(&t.cost).map(String::from)))
            .collect()
    };
    let row = |uid: i32, work: i64, cost: &str| (uid, Some(work), Some(cost.to_string()));
    ed.set_duration_min(1, 960).unwrap();
    // Task 1's assignment: work 480 → 960, cost 1 → 90000.
    let edited = vec![
        row(0, 1440, "93001"),
        row(3, 1440, "93001"),
        row(1, 960, "91500"),
        row(2, 480, "1501"),
    ];
    assert_eq!(totals(&ed), edited);
    // A second refresh of the same edit changes nothing.
    ed.reschedule();
    assert_eq!(totals(&ed), edited);
    // Deleting task 2 takes its assignment (work 480, cost 1) out of the
    // summaries above it.
    ed.delete_task(2).unwrap();
    assert_eq!(
        totals(&ed),
        [
            row(0, 960, "93000"),
            row(3, 960, "93000"),
            row(1, 960, "91500")
        ]
    );
    // Deleting summary 3 takes both its tasks' assignments (work 960 + 480,
    // cost 90000 + 1) out of the project summary, which keeps the rest.
    assert!(ed.undo());
    assert_eq!(totals(&ed), edited);
    ed.delete_task(3).unwrap();
    assert_eq!(totals(&ed), [row(0, 0, "3000")]);
}

#[test]
fn a_task_moved_in_the_outline_takes_its_totals_along() {
    // The project summary (UID 0) over summary 3 (over task 1) and task 2.
    let level = |mut t: Task, outline_level: u32, work: i64, cost: &str| {
        t.outline_level = outline_level;
        t.work_min = Some(work);
        t.cost = Rate::parse(cost);
        t
    };
    let mut ed = staffed();
    ed.proj.tasks = vec![
        level(task(0, 0), 0, 960, "3002"),
        level(task(3, 0), 1, 480, "1501"),
        level(task(1, 1), 2, 480, "1501"),
        level(task(2, 1), 1, 480, "1501"),
    ];
    ed = Editor::new(ed.proj);
    let totals = |ed: &Editor| -> Vec<(i32, Option<i64>, Option<String>)> {
        ed.project()
            .tasks
            .iter()
            .map(|t| (t.uid, t.work_min, text(&t.cost).map(String::from)))
            .collect()
    };
    let row = |uid: i32, work: i64, cost: &str| (uid, Some(work), Some(cost.to_string()));
    let before = totals(&ed);
    // Indented under summary 3, task 2 brings its assignment's work 480 and
    // (stale) cost 1; the project summary already counted it.
    ed.indent(2, 1).unwrap();
    assert_eq!(
        totals(&ed),
        [
            row(0, 960, "3002"),
            row(3, 960, "1502"),
            row(1, 480, "1501"),
            row(2, 480, "1501"),
        ]
    );
    ed.indent(2, -1).unwrap();
    assert_eq!(totals(&ed), before);
}

#[test]
fn a_units_edit_keeps_a_cost_resources_cost() {
    // Summary 3 over task 1, which carries a $500 Pool (cost resource) charge.
    let mut ed = staffed();
    ed.proj.resources.push(Resource {
        uid: 4,
        id: 4,
        name: "Pool".into(),
        kind: ResourceType::Cost,
        max_units: 1.0,
        ..Resource::default()
    });
    let mut summary = task(3, 0);
    let mut leaf = task(1, 1);
    leaf.outline_level = 2;
    for t in [&mut summary, &mut leaf] {
        t.cost = Rate::parse("50000");
    }
    ed.proj.tasks = vec![summary, leaf];
    ed.proj.assignments = vec![Assignment {
        uid: 5,
        task_uid: 1,
        resource_uid: 4,
        units: 1.0,
        cost: Rate::parse("50000"),
        remaining_cost: Rate::parse("50000"),
        ..Assignment::default()
    }];
    ed = Editor::new(ed.proj);
    ed.assign_resource(1, "Pool[50%]").unwrap();
    let a = assignment(&ed, 5);
    assert_eq!(a.units, 0.5);
    assert_eq!(
        (text(&a.cost), text(&a.remaining_cost)),
        (Some("50000"), Some("50000"))
    );
    for t in &ed.project().tasks {
        assert_eq!(text(&t.cost), Some("50000"), "{}", t.uid);
    }
    assert_eq!(text(&ed.project().resources[1].cost), Some("50000"));
}
