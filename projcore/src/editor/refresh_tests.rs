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
