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
        .and_then(|c| c.page)
        .unwrap();
    t.dialogs.select_tab(TABS[page]).unwrap();
    t.dialogs
        .set(name, &Json::obj(vec![("value", value)]))
        .unwrap();
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
        ("predecessors", COL_PREDECESSORS),
        ("resources", COL_RESOURCES),
    ] {
        assert_eq!(read(&t, name), cell_edit_text(ed, task, col), "{name}");
    }
    assert_eq!(read(&t, "name"), "Second");
    assert_eq!(read(&t, "duration"), "2d");
    assert_eq!(read(&t, "predecessors"), "1FS+1d");
    assert_eq!(read(&t, "resources"), "Ann, Bo");
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
    put(&mut t, "predecessors", "1");
    put(&mut t, "resources", "Ann");
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
    assert_eq!(task.deadline, Some(DateTime::from_ymd_hm(2026, 5, 1, 0, 0)));
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
        ("predecessors", "99"),
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
