use super::*;
use crate::dialog_host::{dialog_click, dialog_key};
use core::prelude::v1::test;
use ctlcore::json::Json;
use projcore::DateTime;

fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!("project")
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!("project")
    };
    v
}

/// Tasks First (uid 1, ID 1) and Second (uid 2, ID 2), Second selected.
fn tab() -> DocTab {
    let mut t = new_project_tab("Project1");
    for (name, duration) in [("First", 480), ("Second", 960)] {
        vm(&mut t).ed.add_task(None, name, duration, false).unwrap();
    }
    let p = v(&t).ed.project().clone();
    t.surface = Surface::Project(ProjectView::new(p, false));
    vm(&mut t).ed.select(1);
    t
}

fn open(t: &mut DocTab, act: ProjectAct) {
    apply_project_act(t, act);
    assert_eq!(t.dialogs.top_id(), "task-information");
}

/// Show the control's page, as a person does, then set it.
fn set(t: &mut DocTab, name: &str, value: Json) {
    let page = t
        .dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .page;
    // The top row (Name, Duration, Estimated) is on every page.
    if let Some(page) = page {
        t.dialogs.select_tab(TABS[page]).unwrap();
    }
    t.dialogs
        .set(name, &Json::obj(vec![("value", value)]))
        .unwrap();
}

/// Set a grid cell (`name[row].column`) on the grid's page.
fn cell(t: &mut DocTab, address: &str, value: &str) {
    let name = address.split('[').next().unwrap();
    let page = t
        .dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.page)
        .unwrap();
    t.dialogs.select_tab(TABS[page]).unwrap();
    t.dialogs
        .set(
            address,
            &Json::obj(vec![("value", Json::Str(value.into()))]),
        )
        .unwrap();
}

fn rows_of<const N: usize>(rows: &[[&str; N]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|c| c.to_string()).collect())
        .collect()
}

/// A grid's rows, as strings.
fn grid(t: &DocTab, name: &str) -> Vec<Vec<String>> {
    match t.dialogs.top().unwrap().value(name) {
        Some(crate::dialog::Value::Rows(rows)) => rows.clone(),
        other => panic!("{name} is not a grid: {other:?}"),
    }
}

fn put(t: &mut DocTab, name: &str, value: &str) {
    set(t, name, Json::Str(value.into()));
}

fn read(t: &DocTab, name: &str) -> String {
    t.dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .text()
}

/// The plan and its history, to compare a refused or unchanged OK against.
fn state(t: &DocTab) -> (projcore::Project, usize, usize, bool) {
    let ed = &v(t).ed;
    (
        ed.project().clone(),
        ed.undo_depth(),
        ed.redo_depth(),
        t.dirty,
    )
}

#[test]
fn information_opens_on_general_and_notes_on_the_notes_page() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.title, "Task Information");
    assert_eq!(
        d.tabs,
        ["General", "Predecessors", "Resources", "Advanced", "Notes"]
    );
    assert_eq!(d.tab, 0);
    assert_eq!(d.owner, DialogOwner::TaskInfo { uid: 2 });
    assert!(v(&t).prompt.is_none(), "the one-line prompt is gone");
    t.dialogs.clear();
    open(&mut t, ProjectAct::Notes);
    assert_eq!(t.dialogs.top().unwrap().tab, 4);
}

#[test]
fn the_entry_row_and_a_blank_row_open_nothing() {
    for act in [ProjectAct::Constraint, ProjectAct::Notes] {
        let mut t = tab();
        let i = vm(&mut t).ed.insert_blank_row(Some(2)).unwrap();
        vm(&mut t).ed.select(i);
        apply_project_act(&mut t, act);
        assert!(!t.dialogs.is_open(), "{act:?} on a blank row");
        vm(&mut t).enter_entry_row();
        apply_project_act(&mut t, act);
        assert!(!t.dialogs.is_open(), "{act:?} on the entry row");
        assert!(v(&t).prompt.is_none());
    }
}

#[test]
fn the_pages_open_prefilled_in_the_grid_cells_grammar() {
    let mut t = tab();
    {
        let ed = &mut vm(&mut t).ed;
        ed.set_notes(2, "Line 1\nLine 2").unwrap();
        ed.set_predecessors(2, parse_preds(ed, "1FS+1d")).unwrap();
        ed.set_resources(2, &["Ann".into(), "Bo".into()]).unwrap();
        ed.set_constraint(2, "SNET 2026-02-02").unwrap();
        ed.set_percent_complete(2, 30).unwrap();
        ed.set_priority(2, 800).unwrap();
        ed.set_deadline(2, Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0)))
            .unwrap();
        ed.set_task_type(2, TaskType::FixedWork).unwrap();
        ed.set_effort_driven(2, true).unwrap();
    }
    open(&mut t, ProjectAct::Constraint);
    let ed = &v(&t).ed;
    let task = ed.project().task(2).unwrap();
    for (name, col) in [
        ("name", COL_NAME),
        ("duration", COL_DURATION),
        ("start", COL_START),
        ("finish", COL_FINISH),
    ] {
        assert_eq!(read(&t, name), cell_edit_text(ed, task, col), "{name}");
    }
    assert_eq!(read(&t, "name"), "Second");
    assert_eq!(read(&t, "duration"), "2d");
    assert_eq!(
        grid(&t, "predecessors"),
        rows_of(&[
            ["1", "First", "Finish-to-Start (FS)", "1d"],
            ["", "", "", ""]
        ])
    );
    let cost = |k: usize| {
        let a = &ed.project().assignments[k];
        projcore::editor::read_assignment_field(ed, a, projcore::editor::AssignmentField::Cost).text
    };
    assert_eq!(
        grid(&t, "resources"),
        vec![
            vec!["Ann".to_string(), String::new(), "100%".into(), cost(0)],
            vec!["Bo".to_string(), String::new(), "100%".into(), cost(1)],
            vec![String::new(); 4],
        ]
    );
    assert_eq!(read(&t, "percent"), "30");
    assert_eq!(read(&t, "priority"), "800");
    assert_eq!(read(&t, "ctype"), "Start No Earlier Than");
    assert_eq!(read(&t, "cdate"), "2026-02-02");
    assert_eq!(read(&t, "deadline"), "2026-03-02");
    assert_eq!(read(&t, "ttype"), "Fixed Work");
    assert_eq!(read(&t, "effort"), "checked");
    assert_eq!(read(&t, "notes"), "Line 1\\nLine 2");
}

fn parse_preds(ed: &ProjectEditor, text: &str) -> Vec<projcore::Predecessor> {
    let task = ed.project().task(2).unwrap();
    projcore::editor::parse_task_predecessors(text, task, ed.project()).unwrap()
}

#[test]
fn an_unchanged_ok_changes_nothing_and_stays_clean() {
    let mut t = tab();
    vm(&mut t).ed.set_notes(2, "a\nb").unwrap();
    vm(&mut t).ed.set_constraint(2, "MSO 2026-02-02").unwrap();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    assert_eq!(state(&t), before);
    assert!(!v(&t).ed.dirty());
}

#[test]
fn ok_writes_every_changed_field_as_one_undo_step() {
    let mut t = tab();
    let before = state(&t).0;
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "name", "Renamed");
    put(&mut t, "duration", "3d");
    put(&mut t, "percent", "40");
    put(&mut t, "priority", "700");
    cell(&mut t, "predecessors[0].id", "1");
    cell(&mut t, "resources[0].resource name", "Ann");
    put(&mut t, "ctype", "Finish No Later Than");
    put(&mut t, "cdate", "2026-04-01");
    put(&mut t, "deadline", "2026-05-01");
    put(&mut t, "ttype", "Fixed Duration");
    set(&mut t, "effort", Json::Bool(true));
    put(&mut t, "notes", "first\\nsecond");
    dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    let task = v(&t).ed.project().task(2).unwrap().clone();
    assert_eq!(task.name, "Renamed");
    assert_eq!(task.duration_min, 3 * 480);
    assert_eq!(task.percent_complete, Some(40));
    assert_eq!(task.priority, Some(700));
    assert_eq!(task.predecessors.len(), 1);
    assert_eq!(task.constraint, projcore::ConstraintType::FinishNoLaterThan);
    assert_eq!(
        task.deadline,
        Some(DateTime::from_ymd_hm(2026, 5, 1, 17, 0)),
        "a bare date is the end of that working day"
    );
    assert_eq!(task.task_type, Some(TaskType::FixedDuration));
    assert_eq!(task.effort_driven, Some(true));
    assert_eq!(task.notes.as_deref(), Some("first\nsecond"));
    assert_eq!(v(&t).ed.project().resources.len(), 1);
    assert_eq!(v(&t).ed.undo_depth(), 1, "one undo step for every field");
    assert!(t.dirty);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
}

#[test]
fn only_the_changed_fields_are_written() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "priority", "900");
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!(task.priority, Some(900));
    // Start, Finish and the constraint were not touched, so no SNET appears.
    assert_eq!(task.constraint, projcore::ConstraintType::AsSoonAsPossible);
    assert_eq!(task.percent_complete, None);
    assert_eq!(task.task_type, None);
}

#[test]
fn a_refused_field_keeps_the_dialog_open_and_applies_nothing() {
    for (field, bad) in [
        ("duration", "x"),
        ("percent", "101"),
        ("priority", "1001"),
        ("start", "tomorrow"),
        ("deadline", "soon"),
    ] {
        let mut t = tab();
        let before = state(&t);
        open(&mut t, ProjectAct::Constraint);
        // A valid change first, which the refusal must take back with it.
        put(&mut t, "name", "Renamed");
        put(&mut t, "notes", "kept out");
        put(&mut t, field, bad);
        let err = dialog_click(&mut t, "OK").unwrap_err();
        assert!(!err.is_empty(), "{field}");
        assert_eq!(t.dialogs.top_id(), "task-information", "{field}");
        assert_eq!(state(&t), before, "{field} {bad}");
        assert!(!v(&t).ed.dirty(), "{field}");
    }
}

#[test]
fn a_dated_constraint_without_a_date_is_refused() {
    let mut t = tab();
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "ctype", "Must Start On");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("MSO"), "{err}");
    assert_eq!(t.dialogs.top_id(), "task-information");
    assert_eq!(state(&t), before);
}

#[test]
fn the_constraint_date_follows_the_constraint_type() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    let enabled = |t: &DocTab| {
        t.dialogs
            .top()
            .unwrap()
            .controls
            .iter()
            .find(|c| c.name == "cdate")
            .unwrap()
            .enabled
    };
    assert!(!enabled(&t), "As Soon As Possible takes no date");
    put(&mut t, "ctype", "Start No Earlier Than");
    assert!(enabled(&t));
    put(&mut t, "ctype", "As Late As Possible");
    assert!(!enabled(&t));
}

#[test]
fn an_auto_summary_cannot_edit_what_rolls_up_from_its_subtasks() {
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.select(0);
    open(&mut t, ProjectAct::Constraint);
    let d = t.dialogs.top().unwrap();
    for name in ["duration", "start", "finish", "percent"] {
        let c = d.controls.iter().find(|c| c.name == name).unwrap();
        assert!(!c.enabled, "{name}");
    }
    for name in ["name", "priority", "deadline", "notes"] {
        let c = d.controls.iter().find(|c| c.name == name).unwrap();
        assert!(c.enabled, "{name}");
    }
}

#[test]
fn escape_and_cancel_change_nothing() {
    let mut t = tab();
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "name", "Renamed");
    assert!(dialog_key(&mut t, "escape", None, Modifiers::default()));
    assert!(!t.dialogs.is_open());
    assert_eq!(state(&t), before);
    open(&mut t, ProjectAct::Notes);
    put(&mut t, "notes", "gone");
    dialog_click(&mut t, "Cancel").unwrap();
    assert_eq!(state(&t), before);
}

#[test]
fn clearing_the_notes_and_the_deadline_removes_them() {
    let mut t = tab();
    vm(&mut t).ed.set_notes(2, "docs").unwrap();
    vm(&mut t)
        .ed
        .set_deadline(2, Some(DateTime::from_ymd_hm(2026, 3, 2, 17, 0)))
        .unwrap();
    open(&mut t, ProjectAct::Notes);
    put(&mut t, "notes", "");
    put(&mut t, "deadline", "");
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!((task.notes.clone(), task.deadline), (None, None));
}

#[test]
fn a_double_click_on_a_task_id_opens_it_and_a_single_click_does_not() {
    let mut t = tab();
    project_cell_click(&mut t, 1, Some(COL_ID), false);
    assert!(!t.dialogs.is_open());
    project_cell_click(&mut t, 1, Some(COL_ID), true);
    let d = t.dialogs.top().unwrap();
    assert_eq!((d.id.as_str(), d.tab), ("task-information", 0));
    assert_eq!(d.owner, DialogOwner::TaskInfo { uid: 2 });
    assert!(v(&t).cell.is_none(), "the ID cell opens no editor");
    // Another column's double-click still edits the cell.
    t.dialogs.clear();
    project_cell_click(&mut t, 1, Some(COL_NAME), true);
    assert!(!t.dialogs.is_open());
    assert!(v(&t).cell.is_some());
}

fn plain_task(t: &DocTab) -> projcore::Task {
    v(t).ed.project().task(2).unwrap().clone()
}

#[test]
fn a_task_finishing_on_its_deadline_day_does_not_miss_it() {
    let mut t = tab();
    // Second is 2d from Mon 2026-01-05: it finishes Tue 2026-01-06 17:00.
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "deadline", "2026-01-06");
    dialog_click(&mut t, "OK").unwrap();
    let task = plain_task(&t);
    let finish = v(&t).ed.disp_finish(2).unwrap();
    assert_eq!(task.deadline, Some(finish), "the day's last working time");
    assert!(!task.misses_deadline(finish));
    // The day before it does miss.
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "deadline", "2026-01-05");
    dialog_click(&mut t, "OK").unwrap();
    assert!(plain_task(&t).misses_deadline(finish));
}

#[test]
fn a_deadline_with_a_time_keeps_it_and_the_field_round_trips() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "deadline", "2026-03-02T12:30");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(
        plain_task(&t).deadline,
        Some(DateTime::from_ymd_hm(2026, 3, 2, 12, 30))
    );
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "deadline"), "2026-03-02T12:30");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(
        v(&t).ed.undo_depth(),
        1,
        "an untouched field writes nothing"
    );
    // The end of the day shows as the bare date.
    t.dialogs.clear();
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "deadline", "2026-03-02");
    dialog_click(&mut t, "OK").unwrap();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "deadline"), "2026-03-02");
    put(&mut t, "deadline", "2026-03-02T25:00");
    assert!(dialog_click(&mut t, "OK").is_err());
}

#[test]
fn start_and_finish_together_are_refused_on_an_auto_task_and_taken_on_a_manual_one() {
    let mut t = tab();
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "start", "2026-02-02");
    put(&mut t, "finish", "2026-02-06");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("not both"), "{err}");
    assert_eq!(t.dialogs.top_id(), "task-information");
    assert_eq!(state(&t), before);
    t.dialogs.clear();
    vm(&mut t).ed.set_manual(2, true).unwrap();
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "start", "2026-02-02");
    put(&mut t, "finish", "2026-02-06");
    dialog_click(&mut t, "OK").unwrap();
    let ed = &v(&t).ed;
    assert_eq!(ed.disp_start(2).unwrap().parts().day, 2);
    assert_eq!(ed.disp_finish(2).unwrap().parts().day, 6);
}

#[test]
fn a_type_only_change_applies_what_the_field_shows() {
    use projcore::ConstraintType::*;
    // SNET at 08:00 shows the bare date: Must Finish On means that day's end.
    let mut t = tab();
    let start = DateTime::from_ymd_hm(2026, 3, 5, 8, 0);
    vm(&mut t)
        .ed
        .set_constraint_typed(2, StartNoEarlierThan, Some(start))
        .unwrap();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "cdate"), "2026-03-05");
    put(&mut t, "ctype", "Must Finish On");
    dialog_click(&mut t, "OK").unwrap();
    let task = plain_task(&t);
    assert_eq!(task.constraint, MustFinishOn);
    assert_eq!(
        task.constraint_date,
        Some(DateTime::from_ymd_hm(2026, 3, 5, 17, 0))
    );
    // A time shown keeps its instant: FNET 17:00 -> MFO.
    let mut t = tab();
    let at = DateTime::from_ymd_hm(2026, 3, 5, 17, 0);
    vm(&mut t)
        .ed
        .set_constraint_typed(2, FinishNoEarlierThan, Some(at))
        .unwrap();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "cdate"), "2026-03-05T17:00");
    put(&mut t, "ctype", "Must Finish On");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(plain_task(&t).constraint_date, Some(at));
}

#[test]
fn a_typed_bare_date_gets_the_start_or_end_of_the_day_for_its_type() {
    for (kind, hour) in [
        ("Finish No Later Than", 17),
        ("Must Finish On", 17),
        ("Start No Later Than", 8),
        ("Must Start On", 8),
    ] {
        let mut t = tab();
        open(&mut t, ProjectAct::Constraint);
        put(&mut t, "ctype", kind);
        put(&mut t, "cdate", "2026-03-05");
        dialog_click(&mut t, "OK").unwrap();
        let date = plain_task(&t).constraint_date.unwrap();
        assert_eq!(date, DateTime::from_ymd_hm(2026, 3, 5, hour, 0), "{kind}");
        // And what it shows now is the bare date.
        open(&mut t, ProjectAct::Constraint);
        assert_eq!(read(&t, "cdate"), "2026-03-05", "{kind}");
    }
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "ctype", "Must Start On");
    put(&mut t, "cdate", "2026-03-05T10:15");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(
        plain_task(&t).constraint_date,
        Some(DateTime::from_ymd_hm(2026, 3, 5, 10, 15))
    );
}

#[test]
fn unchecking_effort_driven_applies_before_a_resource_is_added() {
    let mut t = tab();
    {
        let ed = &mut vm(&mut t).ed;
        ed.set_effort_driven(2, true).unwrap();
        ed.set_resources(2, &["Ann".into()]).unwrap();
    }
    let duration = plain_task(&t).duration_min;
    open(&mut t, ProjectAct::Constraint);
    cell(&mut t, "resources[1].resource name", "Bo");
    set(&mut t, "effort", Json::Bool(false));
    dialog_click(&mut t, "OK").unwrap();
    let task = plain_task(&t);
    assert_eq!(task.effort_driven, Some(false));
    assert_eq!(task.duration_min, duration, "no longer effort driven");
    assert_eq!(v(&t).ed.project().assignments.len(), 2);
}

#[test]
fn a_percent_that_contradicts_the_actuals_is_refused() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    p.tasks[1].actual_start = Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0));
    vm(&mut t).ed = ProjectEditor::new(p);
    vm(&mut t).ed.select(1);
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "name", "Renamed");
    put(&mut t, "percent", "0");
    // Already 0 shown: unchanged, so nothing to refuse.
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(plain_task(&t).name, "Renamed");
    vm(&mut t).ed.set_percent_complete(2, 20).unwrap();
    let before = (state(&t), before);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "percent", "0");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("actual start"), "{err}");
    assert_eq!(state(&t), before.0);
}

fn tab_named(t: &DocTab) -> String {
    t.dialogs.top().unwrap().tabs[t.dialogs.top().unwrap().tab].clone()
}

#[test]
fn the_top_row_is_one_value_on_every_page() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    let top = ["name", "duration", "estimated"];
    for name in top {
        let c = t
            .dialogs
            .top()
            .unwrap()
            .controls
            .iter()
            .find(|c| c.name == name)
            .unwrap();
        assert_eq!(c.page, None, "{name} sits on every page");
    }
    put(&mut t, "name", "Renamed");
    for tab in TABS {
        t.dialogs.select_tab(tab).unwrap();
        let shown: Vec<_> = t
            .dialogs
            .top()
            .unwrap()
            .page_controls()
            .map(|c| c.name)
            .collect();
        for name in top {
            assert!(shown.contains(&name), "{name} on {tab}");
        }
        assert_eq!(read(&t, "name"), "Renamed", "{tab}");
    }
    assert_eq!(tab_named(&t), "Notes");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(v(&t).ed.project().task(2).unwrap().name, "Renamed");
    assert_eq!(v(&t).ed.undo_depth(), 1);
}

#[test]
fn estimated_is_the_question_mark_on_the_duration() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "estimated"), "unchecked");
    set(&mut t, "estimated", Json::Bool(true));
    assert_eq!(read(&t, "duration"), "2d?");
    // Typing the mark ticks the box, and removing it clears the box.
    put(&mut t, "duration", "3d");
    assert_eq!(read(&t, "estimated"), "unchecked");
    put(&mut t, "duration", "3d?");
    assert_eq!(read(&t, "estimated"), "checked");
    set(&mut t, "estimated", Json::Bool(false));
    assert_eq!(read(&t, "duration"), "3d");
    set(&mut t, "estimated", Json::Bool(true));
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!((task.duration_min, task.estimated), (3 * 480, Some(true)));
    // An estimated duration opens with its box ticked, and clearing it
    // writes a committed duration.
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "estimated"), "checked");
    set(&mut t, "estimated", Json::Bool(false));
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(v(&t).ed.project().task(2).unwrap().estimated, Some(false));
    // Nothing to estimate: the box refuses to tick a blank duration.
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "duration", "");
    set(&mut t, "estimated", Json::Bool(true));
    assert_eq!(read(&t, "estimated"), "unchecked");
}

#[test]
fn schedule_mode_and_inactive_apply_and_a_refusal_takes_the_rest_back() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "mode"), "Auto Scheduled");
    put(&mut t, "mode", "Manually Scheduled");
    set(&mut t, "inactive", Json::Bool(true));
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert!(task.manual && !task.is_active());
    assert_eq!(v(&t).ed.undo_depth(), 1);
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "mode"), "Manually Scheduled");
    assert_eq!(read(&t, "inactive"), "checked");
    set(&mut t, "inactive", Json::Bool(false));
    put(&mut t, "mode", "Auto Scheduled");
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert!(!task.manual && task.is_active());
    // A task under an inactive summary cannot be made active again.
    let mut t = tab();
    vm(&mut t).ed.indent(2, 1).unwrap();
    vm(&mut t).ed.set_active(1, false).unwrap();
    vm(&mut t).ed.select(2);
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(read(&t, "inactive"), "checked");
    set(&mut t, "inactive", Json::Bool(false));
    put(&mut t, "name", "Kept out");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("inactive"), "{err}");
    assert_eq!(t.dialogs.top_id(), "task-information");
    assert_eq!(state(&t), before);
}

#[test]
fn display_on_timeline_hide_bar_and_rollup_apply() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    for name in ["ontimeline", "hidebar", "rollup"] {
        assert_eq!(read(&t, name), "unchecked", "{name}");
    }
    set(&mut t, "ontimeline", Json::Bool(true));
    set(&mut t, "hidebar", Json::Bool(true));
    set(&mut t, "rollup", Json::Bool(true));
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!(
        (task.display_on_timeline, task.hide_bar, task.rollup),
        (Some(true), Some(true), Some(true))
    );
    assert_eq!(v(&t).ed.undo_depth(), 1);
    open(&mut t, ProjectAct::Constraint);
    for name in ["ontimeline", "hidebar", "rollup"] {
        assert_eq!(read(&t, name), "checked", "{name}");
    }
    set(&mut t, "hidebar", Json::Bool(false));
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!(
        (task.display_on_timeline, task.hide_bar, task.rollup),
        (Some(true), Some(false), Some(true))
    );
}

#[test]
fn start_and_finish_are_date_combos_that_pick_from_a_calendar() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    let d = t.dialogs.top().unwrap();
    for name in ["start", "finish"] {
        let c = d.controls.iter().find(|c| c.name == name).unwrap();
        assert_eq!(c.kind, ControlKind::DateCombo, "{name}");
    }
    assert!(
        d.controls
            .iter()
            .find(|c| c.name == "finish")
            .unwrap()
            .beside
    );
    let i = d.controls.iter().position(|c| c.name == "start").unwrap();
    let d = t.dialogs.top_dialog_mut().unwrap();
    d.toggle_calendar(i).unwrap();
    let opened_on = d.calendar.unwrap();
    assert_eq!(
        opened_on.text(),
        read_text(d, "start"),
        "opens on the field"
    );
    d.calendar_move(1, true).unwrap();
    // A Wednesday: a weekend pick would start on the Monday after.
    d.calendar_day(18, true).unwrap();
    let picked = read(&t, "start");
    assert!(picked.ends_with("-18"), "{picked}");
    dialog_click(&mut t, "OK").unwrap();
    let ed = &v(&t).ed;
    let task = ed.project().task(2).unwrap();
    assert_eq!(cell_edit_text(ed, task, COL_START), picked);
}

fn read_text(d: &Dialog, name: &str) -> String {
    d.controls.iter().find(|c| c.name == name).unwrap().text()
}

#[test]
fn help_says_what_f1_says_and_leaves_the_dialog_open() {
    let mut t = tab();
    open(&mut t, ProjectAct::Constraint);
    let before = state(&t);
    let help = t
        .dialogs
        .top()
        .unwrap()
        .buttons
        .iter()
        .find(|b| b.id == "help")
        .expect("a Help button");
    assert!(help.enabled);
    dialog_click(&mut t, "help").unwrap();
    assert_eq!(t.dialogs.top_id(), "task-information");
    assert_eq!(t.status.to_string(), crate::help_tab::DOCS_COMING);
    assert_eq!(state(&t), before, "Help writes nothing");
}

/// Tasks First, Second (selected, uid 2) and Third.
fn tab3() -> DocTab {
    let mut t = tab();
    vm(&mut t).ed.add_task(None, "Third", 480, false).unwrap();
    let p = v(&t).ed.project().clone();
    t.surface = Surface::Project(ProjectView::new(p, false));
    vm(&mut t).ed.select(1);
    t
}

fn preds(t: &DocTab, uid: i32) -> Vec<(i32, projcore::LinkType, i64)> {
    v(t).ed
        .project()
        .task(uid)
        .unwrap()
        .predecessors
        .iter()
        .map(|p| (p.uid, p.link, p.lag))
        .collect()
}

#[test]
fn the_predecessors_page_is_a_grid_of_the_links_and_a_blank_row() {
    let mut t = tab3();
    open(&mut t, ProjectAct::Constraint);
    let d = t.dialogs.top().unwrap();
    let c = d
        .controls
        .iter()
        .find(|c| c.name == "predecessors")
        .unwrap();
    assert_eq!(c.kind, ControlKind::Grid);
    assert!(c.editable_grid);
    assert_eq!(c.columns, ["ID", "Task Name", "Type", "Lag"]);
    assert_eq!(
        c.column_kinds,
        [
            ControlKind::Text,
            ControlKind::Combo,
            ControlKind::Dropdown,
            ControlKind::Text
        ]
    );
    // Every other task, by name; the edited one is not offered.
    assert_eq!(c.column_items[1], ["First", "Third"]);
    assert_eq!(c.column_items[0], ["1", "3"]);
    assert_eq!(
        c.column_items[2],
        [
            "Finish-to-Start (FS)",
            "Start-to-Start (SS)",
            "Finish-to-Finish (FF)",
            "Start-to-Finish (SF)"
        ]
    );
    assert_eq!(grid(&t, "predecessors"), rows_of(&[["", "", "", ""]]));
}

#[test]
fn a_row_added_edited_and_deleted_is_applied_as_one_undo_step() {
    let mut t = tab3();
    open(&mut t, ProjectAct::Constraint);
    // Picking the name fills the ID and the usual type.
    cell(&mut t, "predecessors[0].task name", "first");
    assert_eq!(
        grid(&t, "predecessors")[0],
        ["1", "First", "Finish-to-Start (FS)", ""]
    );
    cell(&mut t, "predecessors[0].lag", "2d");
    // Typing an ID fills the name.
    cell(&mut t, "predecessors[1].id", "3");
    assert_eq!(grid(&t, "predecessors")[1][1], "Third");
    cell(&mut t, "predecessors[1].type", "ss");
    cell(&mut t, "predecessors[1].lag", "-1d");
    assert_eq!(grid(&t, "predecessors").len(), 3, "a blank row follows");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(
        preds(&t, 2),
        [
            (1, projcore::LinkType::FinishStart, 2 * 480),
            (3, projcore::LinkType::StartStart, -480)
        ]
    );
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // Reopened, the rows read back as they were entered.
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(
        grid(&t, "predecessors"),
        rows_of(&[
            ["1", "First", "Finish-to-Start (FS)", "2d"],
            ["3", "Third", "Start-to-Start (SS)", "-1d"],
            ["", "", "", ""]
        ])
    );
    // Delete removes the selected row.
    t.dialogs.select_tab("Predecessors").unwrap();
    t.dialogs.select_row("predecessors", 0).unwrap();
    assert!(dialog_key(
        &mut t,
        "delete",
        None,
        gpui::Modifiers::default()
    ));
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(preds(&t, 2), [(3, projcore::LinkType::StartStart, -480)]);
    assert_eq!(v(&t).ed.undo_depth(), 2);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(preds(&t, 2).len(), 2);
}

#[test]
fn a_link_that_would_make_a_cycle_is_refused_and_the_dialog_stays_open() {
    let mut t = tab3();
    vm(&mut t)
        .ed
        .set_predecessors(2, vec![projcore::Predecessor::fs(1)])
        .unwrap();
    vm(&mut t).ed.select(0);
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    put(&mut t, "name", "Kept out");
    cell(&mut t, "predecessors[0].task name", "Second");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(
        err.to_lowercase().contains("circular") || err.to_lowercase().contains("cycle"),
        "{err}"
    );
    assert_eq!(t.dialogs.top_id(), "task-information");
    assert_eq!(state(&t), before);
}

#[test]
fn bad_rows_are_refused_with_what_is_wrong() {
    for (address, value, wants) in [
        ("predecessors[0].id", "9", "No task with ID 9"),
        (
            "predecessors[0].task name",
            "Nobody",
            "No task named 'Nobody'",
        ),
        ("predecessors[0].lag", "soon", "Invalid predecessor lag"),
    ] {
        let mut t = tab3();
        let before = state(&t);
        open(&mut t, ProjectAct::Constraint);
        if address != "predecessors[0].id" && address != "predecessors[0].task name" {
            cell(&mut t, "predecessors[0].id", "1");
        }
        cell(&mut t, address, value);
        let err = dialog_click(&mut t, "OK").unwrap_err();
        assert!(err.contains(wants), "{address}: {err}");
        assert_eq!(state(&t), before, "{address}");
    }
    // A link to the task itself and a link twice.
    let mut t = tab3();
    open(&mut t, ProjectAct::Constraint);
    cell(&mut t, "predecessors[0].id", "2");
    assert!(dialog_click(&mut t, "OK").unwrap_err().contains("itself"));
    cell(&mut t, "predecessors[0].id", "1");
    cell(&mut t, "predecessors[1].id", "1");
    assert!(
        dialog_click(&mut t, "OK")
            .unwrap_err()
            .contains("Duplicate")
    );
}

#[test]
fn a_link_kept_as_shown_keeps_its_stored_form_through_another_rows_edit() {
    let mut t = tab3();
    let kept = projcore::Predecessor {
        cross_project: Some(true),
        cross_project_name: Some("Other.mpp\\4".into()),
        lag: 90,
        lag_format: projcore::LagFormat::from_code(3).unwrap(),
        ..projcore::Predecessor::fs(1)
    };
    vm(&mut t)
        .ed
        .set_predecessors(2, vec![kept.clone()])
        .unwrap();
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(grid(&t, "predecessors")[0][0], "Other.mpp\\4");
    cell(&mut t, "predecessors[1].id", "3");
    dialog_click(&mut t, "OK").unwrap();
    let task = v(&t).ed.project().task(2).unwrap();
    assert_eq!(task.predecessors[0], kept, "the cross link, as it was");
    assert_eq!(task.predecessors[1].uid, 3);
}

fn assigned(t: &DocTab, uid: i32) -> Vec<(String, f64)> {
    let proj = v(t).ed.project();
    proj.assignments
        .iter()
        .filter(|a| a.task_uid == uid)
        .map(|a| {
            let r = proj.resources.iter().find(|r| r.uid == a.resource_uid);
            (r.map_or(String::new(), |r| r.name.clone()), a.units)
        })
        .collect()
}

#[test]
fn the_resources_page_is_a_grid_with_read_only_owner_and_cost() {
    let mut t = tab();
    vm(&mut t)
        .ed
        .set_resources(2, &["Ann".into(), "Bo[50%]".into()])
        .unwrap();
    {
        let proj = v(&t).ed.project().clone();
        let mut p = proj;
        p.assignments[0].assn_owner = Some("Owner One".into());
        vm(&mut t).ed = ProjectEditor::new(p);
        vm(&mut t).ed.select(1);
    }
    open(&mut t, ProjectAct::Constraint);
    let d = t.dialogs.top().unwrap();
    let c = d.controls.iter().find(|c| c.name == "resources").unwrap();
    assert!(c.editable_grid);
    assert_eq!(
        c.columns,
        ["Resource Name", "Assignment Owner", "Units", "Cost"]
    );
    assert_eq!(
        c.column_kinds,
        [
            ControlKind::Combo,
            ControlKind::Label,
            ControlKind::Text,
            ControlKind::Label
        ]
    );
    assert_eq!(c.column_items[0], ["Ann", "Bo"]);
    let rows = grid(&t, "resources");
    assert_eq!(rows.len(), 3, "two assignments and a blank row");
    assert_eq!(
        (&rows[0][0], &rows[0][1], &rows[0][2]),
        (
            &"Ann".to_string(),
            &"Owner One".to_string(),
            &"100%".to_string()
        )
    );
    assert_eq!(rows[1][2], "50%");
    t.dialogs.select_tab("Resources").unwrap();
    for column in ["assignment owner", "cost"] {
        let err = t
            .dialogs
            .set(
                &format!("resources[0].{column}"),
                &Json::obj(vec![("value", Json::Str("x".into()))]),
            )
            .unwrap_err();
        assert!(err.contains("read-only"), "{column}: {err}");
    }
    // Typing into one with the keys starts nothing either.
    t.dialogs.select_row("resources", 0).unwrap();
    let d = t.dialogs.top_dialog_mut().unwrap();
    let i = d.focus.unwrap();
    d.controls[i].col = 3;
    d.grid_key("x", Some("x")).unwrap();
    assert_eq!(d.controls[i].edit, None);
}

#[test]
fn resource_rows_add_a_name_change_units_and_delete() {
    let mut t = tab();
    vm(&mut t).ed.set_resources(2, &["Ann".into()]).unwrap();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let depth = v(&t).ed.undo_depth();
    open(&mut t, ProjectAct::Constraint);
    // A new name gets 100% to start with.
    cell(&mut t, "resources[1].resource name", "Bo");
    assert_eq!(grid(&t, "resources")[1][2], "100%");
    cell(&mut t, "resources[0].units", "50%");
    cell(&mut t, "resources[1].units", "25");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(assigned(&t, 2), [("Ann".into(), 0.5), ("Bo".into(), 0.25)]);
    assert_eq!(v(&t).ed.project().resources.len(), 2, "Bo is new");
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    // Back to 100% resets the units, and a row with no name is ignored.
    open(&mut t, ProjectAct::Constraint);
    assert_eq!(grid(&t, "resources")[0][2], "50%");
    cell(&mut t, "resources[0].units", "100%");
    cell(&mut t, "resources[2].units", "10%");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(assigned(&t, 2), [("Ann".into(), 1.0), ("Bo".into(), 0.25)]);
    // Delete drops the row's assignment.
    open(&mut t, ProjectAct::Constraint);
    t.dialogs.select_tab("Resources").unwrap();
    t.dialogs.select_row("resources", 1).unwrap();
    assert!(dialog_key(
        &mut t,
        "delete",
        None,
        gpui::Modifiers::default()
    ));
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(assigned(&t, 2), [("Ann".into(), 1.0)]);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(assigned(&t, 2).len(), 2);
}

#[test]
fn bad_units_are_refused_and_an_untouched_grid_writes_nothing() {
    let mut t = tab();
    vm(&mut t)
        .ed
        .set_resources(2, &["Ann[50%]".into()])
        .unwrap();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = state(&t);
    open(&mut t, ProjectAct::Constraint);
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(state(&t), before, "units shown as 50% stay 50%");
    open(&mut t, ProjectAct::Constraint);
    cell(&mut t, "resources[0].units", "lots");
    let err = dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("Invalid units"), "{err}");
    assert_eq!(state(&t), before);
    assert_eq!(t.dialogs.top_id(), "task-information");
}
