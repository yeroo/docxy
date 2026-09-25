use super::*;
use crate::model::{Calendar, CalendarException, DayWorking};

/// Monday 2026-01-05 at `hour`, plus `days` calendar days.
fn at(days: i64, hour: u32) -> DateTime {
    DateTime::from_ymd_hm(2026, 1, 5, hour, 0).add_days(days)
}

/// Three unlinked 1-day auto tasks (uids 10, 20, 30) starting Monday 08:00.
fn editor() -> Editor {
    let mut p = untitled_project();
    p.tasks = (1..=3)
        .map(|id| Task {
            id,
            uid: id * 10,
            name: format!("Task {id}"),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        })
        .collect();
    Editor::new(p)
}

fn history(ed: &Editor) -> (usize, usize, bool) {
    (ed.undo_depth(), ed.redo_depth(), ed.dirty())
}

/// An auto task's Move leaves it constrained to start no earlier than `start`,
/// and scheduled there.
fn assert_snet(ed: &Editor, uid: i32, start: DateTime) {
    let t = ed.project().task(uid).unwrap();
    assert_eq!(t.constraint, ConstraintType::StartNoEarlierThan);
    assert_eq!(t.constraint_date, Some(start));
    assert_eq!(ed.schedule().get(uid).unwrap().early_start, start);
}

#[test]
fn move_walks_working_days_on_the_default_calendar() {
    // (start before the move, amount, start after it)
    for (from, text, to) in [
        (at(0, 8), "1d", at(1, 8)),  // Mon -> Tue
        (at(4, 8), "+1d", at(7, 8)), // Fri -> next Mon
        (at(0, 8), "1w", at(7, 8)),  // Mon -> next Mon
        (at(0, 8), "4w", at(28, 8)), // four weeks
        (at(1, 8), "-1d", at(0, 8)), // Tue -> Mon, backward
        (at(7, 8), "-1D", at(4, 8)), // Mon -> Fri, backward over the weekend
    ] {
        let mut ed = editor();
        if from != at(0, 8) {
            ed.set_start_at(10, from).unwrap();
        }
        assert_eq!(ed.schedule().get(10).unwrap().early_start, from);
        let depth = ed.undo_depth();
        assert_eq!(ed.move_task(10, text), Ok(to), "{text} from {from:?}");
        assert_snet(&ed, 10, to);
        assert_eq!(ed.undo_depth(), depth + 1, "one undo step");
        ed.undo();
        assert_eq!(ed.schedule().get(10).unwrap().early_start, from);
    }
}

#[test]
fn move_skips_a_holiday_on_the_tasks_calendar() {
    let mut ed = editor();
    ed.set_start_at(10, at(1, 8)).unwrap();
    let mut proj = ed.project().clone();
    proj.calendars[0]
        .exceptions
        .push(CalendarException::date_range(
            at(2, 0),
            at(2, 23),
            DayWorking::default(),
        ));
    let mut ed = Editor::new(proj);
    assert_eq!(ed.move_task(10, "1d"), Ok(at(3, 8)), "Tue -> Thu over Wed");
    assert_snet(&ed, 10, at(3, 8));
}

#[test]
fn move_keeps_the_time_of_day() {
    // A 4h predecessor ends at noon, so after lunch the task starts at 13:00.
    let mut ed = editor();
    ed.set_duration_min(10, 240).unwrap();
    ed.add_predecessor(20, 10, LinkType::FinishStart, 0)
        .unwrap();
    assert_eq!(ed.schedule().get(20).unwrap().early_start, at(0, 13));
    assert_eq!(ed.move_task(20, "1d"), Ok(at(1, 13)));
    assert_snet(&ed, 20, at(1, 13));
}

#[test]
fn move_replaces_an_auto_tasks_constraint() {
    let mut ed = editor();
    ed.set_constraint(10, "mfo 2026-01-07T17:00:00").unwrap();
    let start = ed.schedule().get(10).unwrap().early_start;
    assert_eq!(start, at(2, 8));
    assert_eq!(ed.move_task(10, "1d"), Ok(at(3, 8)));
    assert_snet(&ed, 10, at(3, 8));
}

#[test]
fn move_ignores_leveling_delay() {
    // Two tasks on one resource: leveling delays the second by a day.
    let crew = || {
        let mut ed = editor();
        ed.assign_resource(10, "Crew").unwrap();
        ed.assign_resource(20, "Crew").unwrap();
        ed
    };
    let (mut ed, mut off) = (crew(), crew());
    ed.toggle_level();
    assert!(ed.disp_start(20).unwrap() > ed.schedule().get(20).unwrap().early_start);
    assert_eq!(ed.move_task(20, "1d"), Ok(at(1, 8)));
    assert_eq!(off.move_task(20, "1d"), Ok(at(1, 8)));
    assert_eq!(
        ed.project().task(20).unwrap().constraint_date,
        off.project().task(20).unwrap().constraint_date
    );
}

#[test]
fn move_shifts_a_manual_tasks_pinned_start_keeping_time_and_duration() {
    let mut ed = editor();
    let mut proj = ed.project().clone();
    let task = &mut proj.tasks[0];
    task.manual = true;
    task.manual_start = Some(at(4, 10));
    task.manual_finish = Some(at(7, 10));
    task.duration_min = 480;
    task.manual_duration_min = Some(480);
    ed = Editor::new(proj);
    assert_eq!(
        ed.move_task(10, "1d"),
        Ok(at(7, 10)),
        "Fri 10:00 -> Mon 10:00"
    );
    let task = ed.project().task(10).unwrap();
    assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
    assert_eq!(task.manual_start, Some(at(7, 10)));
    assert_eq!(task.manual_finish, None);
    assert_eq!(task.duration_min, 480);
    assert_eq!(ed.disp_start(10), Some(at(7, 10)));
    assert_eq!(ed.undo_depth(), 1);
    ed.undo();
    assert_eq!(ed.project().task(10).unwrap().manual_start, Some(at(4, 10)));
}

#[test]
fn refused_moves_change_nothing() {
    let mut ed = editor();
    ed.indent(20, 1).unwrap();
    assert!(ed.project().task(10).unwrap().summary);
    let mut proj = ed.project().clone();
    proj.tasks.push(Task {
        uid: 40,
        id: 4,
        is_null: true,
        ..Task::default()
    });
    let mut ed = Editor::new(proj);
    ed.rename(30, "x").unwrap();
    ed.undo();
    ed.mark_saved();
    let before = ed.project().clone();
    let state = history(&ed);
    for (uid, text, error) in [
        (10, "1d", "Move a subtask, not a summary"),
        (40, "1d", "The row has no task to move"),
        (20, "0d", "Move by at least one day"),
        (20, "1", "Couldn't read '1' (try 1d, 1w, 4w, -1d)"),
        (20, "1.5d", "Couldn't read '1.5d' (try 1d, 1w, 4w, -1d)"),
        (20, "4h", "Couldn't read '4h' (try 1d, 1w, 4w, -1d)"),
        (20, "", "Couldn't read '' (try 1d, 1w, 4w, -1d)"),
        (
            20,
            "99999999999999999999d",
            "Couldn't read '99999999999999999999d' (try 1d, 1w, 4w, -1d)",
        ),
        (
            20,
            "9223372036854775807w",
            "Move is outside the scheduling range",
        ),
        (20, "40000d", "Move is outside the scheduling range"),
    ] {
        assert_eq!(ed.move_task(uid, text), Err(error.into()), "{uid} {text}");
        assert_eq!(ed.project(), &before, "{uid} {text}");
        assert_eq!(history(&ed), state, "{uid} {text}");
    }
    assert!(ed.project().task(40).unwrap().is_null, "not materialized");
}

#[test]
fn move_on_a_calendar_without_working_days_is_refused() {
    let mut ed = editor();
    let mut proj = ed.project().clone();
    proj.calendars
        .push(Calendar::base(7, "Closed", Default::default()));
    let task = &mut proj.tasks[0];
    task.calendar_uid = Some(7);
    task.manual = true;
    task.manual_start = Some(at(0, 8));
    ed = Editor::new(proj);
    let before = ed.project().clone();
    assert_eq!(
        ed.move_task(10, "1d"),
        Err("No working day in reach on the task's calendar".into())
    );
    assert_eq!(ed.project(), &before);
    assert_eq!(history(&ed), (0, 0, false));
}

#[test]
fn a_week_is_the_plans_working_days_per_week() {
    let mut proj = untitled_project();
    assert_eq!(parse_move("1w", &proj), Ok(5));
    assert_eq!(parse_move("-2w", &proj), Ok(-10));
    proj.hours_per_week = 48.0;
    assert_eq!(parse_move("1w", &proj), Ok(6));
    proj.hours_per_week = 37.5;
    proj.hours_per_day = 7.5;
    assert_eq!(parse_move("1w", &proj), Ok(5));
    proj.hours_per_day = 0.0;
    assert_eq!(parse_move("1w", &proj), Ok(5), "unusable hours fall back");
}
