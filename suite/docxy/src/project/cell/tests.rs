use super::*;
use core::prelude::v1::test;
use projcore::ConstraintType;

fn tab() -> DocTab {
    let p = Project {
        start_date: Some(projcore::editor::default_anchor()),
        tasks: (1..=3)
            .map(|id| Task {
                uid: id * 10,
                id,
                name: format!("Task {id}"),
                duration_min: 480,
                ..Task::default()
            })
            .collect(),
        ..Project::default()
    };
    project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    )
}
fn v(t: &DocTab) -> &ProjectView {
    let Surface::Project(v) = &t.surface else {
        panic!()
    };
    v
}
fn vm(t: &mut DocTab) -> &mut ProjectView {
    let Surface::Project(v) = &mut t.surface else {
        panic!()
    };
    v
}
fn key(t: &mut DocTab, key: &str) {
    if let Some(act) = project_input(t, key, None, Modifiers::default()) {
        apply_project_act(t, act);
    }
}
fn edit(t: &mut DocTab, col: usize, text: &str) {
    vm(t).col = col;
    project_input(t, "text", Some(text), Modifiers::default());
}

#[test]
fn navigation_scrolls_and_all_printable_shortcuts_start_an_edit() {
    let mut t = tab();
    assert_eq!(v(&t).col, COL_NAME);
    for c in ["n", "x", "d", "p", "c", "a", "b", "L", "λ"] {
        let modifiers = Modifiers {
            shift: c == "L",
            ..Modifiers::default()
        };
        let key_name = c.to_lowercase();
        assert!(project_input(&mut t, &key_name, Some(c), modifiers).is_none());
        assert_eq!(v(&t).cell.as_ref().unwrap().buf, c);
        key(&mut t, "escape");
        assert!(!t.dirty);
    }
    key(&mut t, "end");
    assert_eq!(v(&t).ed.sel(), 2);
    key(&mut t, "home");
    assert_eq!(v(&t).ed.sel(), 0);
    for _ in 0..10 {
        key(&mut t, "right");
    }
    assert_eq!(v(&t).col, COL_RESOURCES);
    assert!(v(&t).table_x.get() > 0.);
    assert_eq!(v(&t).gantt_x.get(), 0.);
    for _ in 0..10 {
        key(&mut t, "left");
    }
    assert_eq!(v(&t).col, COL_ID);
    assert_eq!(v(&t).table_x.get(), 0.);
    key(&mut t, "f2");
    assert!(v(&t).cell.is_none());
    assert!(t.status.contains("read-only"));
}

#[test]
fn all_columns_commit_as_one_step_and_undo_restores_schedule() {
    for (col, text) in [
        (COL_NAME, "Renamed"),
        (COL_DURATION, "2d"),
        (COL_START, "2026-01-08"),
        (COL_FINISH, "2026-01-09"),
        (COL_PREDECESSORS, "2SS+2h, 3FF-7m"),
        (COL_RESOURCES, "Alice, Bob"),
    ] {
        let mut t = tab();
        let before = v(&t).ed.project().clone();
        let finish = v(&t).ed.disp_finish(10);
        edit(&mut t, col, text);
        key(&mut t, "enter");
        assert!(v(&t).cell.is_none(), "{col}: {}", t.status);
        assert_eq!(v(&t).ed.sel(), 1);
        assert_eq!(v(&t).ed.undo_depth(), 1);
        assert!(t.dirty);
        let after = v(&t).ed.project().clone();
        let after_finish = v(&t).ed.disp_finish(10);
        apply_project_act(&mut t, ProjectAct::Undo);
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.disp_finish(10), finish);
        apply_project_act(&mut t, ProjectAct::Redo);
        assert_eq!(v(&t).ed.project(), &after);
        assert_eq!(v(&t).ed.disp_finish(10), after_finish);
    }
}

#[test]
fn no_op_date_duration_and_milestone_keep_history_and_redo() {
    let mut t = tab();
    vm(&mut t).ed.set_constraint(10, "MSO 2026-01-08").unwrap();
    vm(&mut t).ed.rename(10, "temporary").unwrap();
    vm(&mut t).ed.undo();
    vm(&mut t).ed.mark_saved();
    t.dirty = false;
    let before = v(&t).ed.project().clone();
    for col in COL_MODE..COLUMN_COUNT {
        vm(&mut t).ed.select(0);
        vm(&mut t).col = col;
        key(&mut t, "f2");
        key(&mut t, "enter");
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!((v(&t).ed.undo_depth(), v(&t).ed.redo_depth()), (1, 1));
        assert!(!t.dirty);
    }
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_DURATION, "8h");
    key(&mut t, "enter");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_DURATION, "0");
    key(&mut t, "enter");
    vm(&mut t).ed.select(0);
    key(&mut t, "f2");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "0");
}

#[test]
fn typed_dates_move_a_manual_task_without_constraints() {
    let mut p = v(&tab()).ed.project().clone();
    for task in &mut p.tasks {
        task.manual = true;
        task.manual_start = p.start_date;
    }
    let mut t = project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    );
    let depth = v(&t).ed.undo_depth();
    // Tab commits and stays on the row; Enter would move down.
    edit(&mut t, COL_START, "2026-01-08");
    key(&mut t, "tab");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    edit(&mut t, COL_FINISH, "2026-01-09");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert!(!t.status.contains("Constraint"), "{}", t.status);
    let ed = &v(&t).ed;
    let task = ed.project().task(10).unwrap();
    assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
    assert_eq!(task.duration_min, 960);
    let row = project_row(ed, task);
    assert_eq!(
        (row[COL_START].as_str(), row[COL_FINISH].as_str()),
        ("2026-01-08", "2026-01-09")
    );
    assert_eq!(ed.undo_depth(), depth + 2);
}

#[test]
fn a_date_typed_into_a_blank_row_of_a_manual_plan_pins_it() {
    let mut p = v(&tab()).ed.project().clone();
    p.new_tasks_are_manual = true;
    p.tasks[1] = Task {
        uid: 20,
        id: 2,
        is_null: true,
        ..Task::default()
    };
    let mut t = project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    );
    for (col, text) in [(COL_START, "2026-01-08"), (COL_FINISH, "2026-01-09")] {
        vm(&mut t).ed.select(1);
        edit(&mut t, col, text);
        key(&mut t, "enter");
        assert!(v(&t).cell.is_none(), "{}", t.status);
        // The row became a manual task: no constraint was set or claimed.
        assert!(!t.status.contains("Constraint"), "{}", t.status);
        let ed = &v(&t).ed;
        let task = ed.project().task(20).unwrap();
        assert!(task.manual && !task.is_null);
        assert_eq!(task.constraint, ConstraintType::AsSoonAsPossible);
        assert_eq!(project_row(ed, task)[col], text);
    }
}

#[test]
fn reentering_an_existing_constraint_does_not_claim_a_change() {
    let mut t = tab();
    vm(&mut t).ed.set_constraint(20, "SNET 2026-03-06").unwrap();
    vm(&mut t)
        .ed
        .add_predecessor(10, 20, LinkType::FinishStart, 0)
        .unwrap();
    vm(&mut t).ed.set_constraint(10, "SNET 2026-03-05").unwrap();
    assert_eq!(
        project_row(&v(&t).ed, v(&t).ed.project().task(10).unwrap())[COL_START],
        "2026-03-09"
    );
    let before = v(&t).ed.project().clone();
    let depth = v(&t).ed.undo_depth();
    t.status = "unchanged".into();
    edit(&mut t, COL_START, "2026-03-05");
    key(&mut t, "enter");
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    assert_eq!(t.status, "unchanged");
}

#[test]
fn correcting_a_failed_commit_clears_only_that_editors_error() {
    for (col, invalid, corrected) in [
        (COL_DURATION, "banana", "2d"),
        (COL_DURATION, "banana", "1d"),
        (COL_START, "bad date", "2026-03-05"),
    ] {
        let mut t = tab();
        edit(&mut t, col, invalid);
        key(&mut t, "enter");
        let error = t.status.clone();
        assert_eq!(
            v(&t).cell.as_ref().unwrap().last_error.as_deref(),
            Some(error.as_ref())
        );
        key(&mut t, "home");
        for _ in invalid.chars() {
            key(&mut t, "delete");
        }
        project_input(&mut t, "text", Some(corrected), Modifiers::default());
        key(&mut t, "enter");
        assert!(v(&t).cell.is_none());
        assert_ne!(t.status, error);
        assert_eq!(
            t.status.as_ref(),
            if col == COL_START {
                "Constraint set: SNET (was ASAP)"
            } else {
                "Ready"
            }
        );
        if corrected == "1d" {
            assert!(!t.dirty);
            assert_eq!(v(&t).ed.undo_depth(), 0);
        }
    }
    let mut t = tab();
    edit(&mut t, COL_DURATION, "banana");
    key(&mut t, "enter");
    t.status = "New unrelated status".into();
    vm(&mut t).cell.as_mut().unwrap().buf = "2d".into();
    vm(&mut t).cell.as_mut().unwrap().caret = 2;
    key(&mut t, "enter");
    assert_eq!(t.status, "New unrelated status");
}

#[test]
fn invalid_inputs_and_click_away_preserve_everything() {
    for (col, text) in [
        (COL_DURATION, "NaN"),
        (COL_DURATION, "-1d"),
        (COL_DURATION, "1e50d"),
        (COL_START, "2026-02-31"),
        (COL_FINISH, "2026-01-10"),
        (COL_PREDECESSORS, "1"),
        (COL_PREDECESSORS, "99"),
        (COL_PREDECESSORS, "2,2"),
    ] {
        let mut t = tab();
        let before = v(&t).ed.project().clone();
        edit(&mut t, col, text);
        key(&mut t, "tab");
        assert_eq!(v(&t).cell.as_ref().unwrap().buf, text);
        assert_eq!(v(&t).col, col);
        project_cell_click(&mut t, 1, Some(COL_NAME), true);
        project_cell_click(&mut t, 1, None, false);
        apply_project_act(&mut t, ProjectAct::Rename);
        assert!(!commit_project_cell(&mut t));
        assert!(v(&t).prompt.is_none());
        assert_eq!(v(&t).ed.sel(), 0);
        assert_eq!(v(&t).ed.project(), &before);
        assert_eq!(v(&t).ed.undo_depth(), 0);
        assert!(!t.dirty);
        key(&mut t, "escape");
        assert!(v(&t).cell.is_none());
    }
}

#[test]
fn clicking_below_the_last_task_commits_then_moves_to_the_entry_row() {
    let mut t = tab();
    key(&mut t, "down");
    edit(&mut t, COL_NAME, "Renamed");
    project_below_click(&mut t);
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).ed.project().tasks[1].name, "Renamed");
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 3);
    assert_eq!(v(&t).selected_uid(), None);
    assert_eq!(
        v(&t).col,
        COL_NAME,
        "a click below the entry row keeps the column"
    );
    assert!(t.dirty);
    // With nothing open it only moves the cursor.
    project_below_click(&mut t);
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).cursor_row(), 3);
    assert_eq!(v(&t).ed.undo_depth(), 1);

    let mut t = tab();
    let before = v(&t).ed.project().clone();
    edit(&mut t, COL_DURATION, "NaN");
    project_entry_click(&mut t, Some(COL_NAME), false);
    let status = t.status.clone();
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "NaN");
    assert_eq!(
        v(&t).cell.as_ref().unwrap().last_error.as_deref(),
        Some(status.as_ref())
    );
    assert_eq!(v(&t).ed.sel(), 0);
    assert!(!v(&t).on_entry_row(), "a failed commit keeps the cursor");
    assert_eq!(v(&t).col, COL_DURATION);
    assert_eq!(v(&t).ed.project(), &before);
    assert!(!t.dirty);
}

#[test]
fn clicking_prompts_and_tab_share_commit_policy() {
    let mut t = tab();
    edit(&mut t, COL_NAME, "New");
    project_cell_click(&mut t, 1, Some(COL_DURATION), true);
    assert_eq!(v(&t).ed.project().tasks[0].name, "New");
    assert_eq!(v(&t).cell.as_ref().unwrap().col, COL_DURATION);
    key(&mut t, "escape");
    edit(&mut t, COL_NAME, "Next");
    apply_project_act(&mut t, ProjectAct::Duration);
    assert!(v(&t).cell.is_none() && v(&t).prompt.is_some());
    project_cell_click(&mut t, 0, Some(COL_NAME), false);
    assert!(v(&t).prompt.is_none());
    key(&mut t, "f2");
    project_input(
        &mut t,
        "tab",
        None,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(v(&t).col, COL_MODE);
    assert!(v(&t).cell.is_none());
    key(&mut t, "tab");
    assert_eq!(v(&t).col, COL_NAME);
    edit(&mut t, COL_NAME, "Saved");
    assert_eq!(
        project_input(
            &mut t,
            "s",
            None,
            Modifiers {
                control: true,
                ..Modifiers::default()
            }
        ),
        Some(ProjectAct::Save)
    );
    assert_eq!(v(&t).ed.project().tasks[0].name, "Saved");
}

#[test]
fn summary_cells_are_read_only() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    for col in COL_DURATION..=COL_FINISH {
        edit(&mut t, col, "1");
        assert!(v(&t).cell.is_none());
        assert!(t.status.contains("read-only"));
    }
}

#[test]
fn save_commits_a_cell_and_invalid_input_never_reaches_disk() {
    let mut t = tab();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/cell-save-{}.yppx", std::process::id()));
    edit(&mut t, COL_NAME, "Saved from cell");
    apply_save(&mut t, &path).unwrap();
    assert!(v(&t).cell.is_none());
    assert!(!t.dirty);
    let bytes = std::fs::read(&path).unwrap();
    let saved = project_from_path(&path).unwrap();
    assert_eq!(saved.task(10).unwrap().name, "Saved from cell");
    edit(&mut t, COL_DURATION, "invalid");
    assert!(apply_save(&mut t, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "invalid");
    assert!(!t.dirty);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn caret_edits_utf8_and_long_buffer_window_tracks_it() {
    let mut c = CellEdit {
        last_error: None,
        uid: Some(1),
        col: COL_NAME,
        initial: String::new(),
        buf: "aλ🙂z".into(),
        caret: 8,
    };
    c.key("left", None);
    c.key("backspace", None);
    assert_eq!(c.buf, "aλz");
    assert_eq!(c.caret, 3);
    c.key("left", None);
    c.key("delete", None);
    assert_eq!(c.buf, "az");
    c.key("home", None);
    c.key("text", Some("é"));
    assert_eq!(c.buf, "éaz");
    c.key("end", None);
    assert_eq!(c.caret, c.buf.len());
    c.key("text", Some("123456789"));
    let measure = |s: &str| s.chars().count() as f32 * 10.;
    assert_eq!(c.scroll_x(30., measure), 90.);
    c.key("left", None);
    assert_eq!(c.scroll_x(30., measure), 80.);
    c.key("home", None);
    assert_eq!(c.scroll_x(30., measure), 0.);
}

#[test]
fn resource_names_cell_shows_and_keeps_partial_units() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    for (uid, name, max_units) in [(1, "Bob", 0.5), (2, "Alice", 1.0)] {
        p.resources.push(projcore::model::Resource {
            uid,
            id: uid,
            name: name.into(),
            max_units,
            ..Default::default()
        });
    }
    vm(&mut t).ed.replace_project(p);
    vm(&mut t)
        .ed
        .set_resources(10, &["Bob".into(), "Alice".into()])
        .unwrap();
    let row = |t: &DocTab| {
        project_row(&v(t).ed, v(t).ed.project().task(10).unwrap())[COL_RESOURCES].clone()
    };
    assert_eq!(row(&t), "Bob[50%], Alice");
    let depth = v(&t).ed.undo_depth();
    vm(&mut t).col = COL_RESOURCES;
    key(&mut t, "f2");
    project_input(&mut t, "text", Some(", Carol"), Modifiers::default());
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "Bob[50%], Alice, Carol");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.undo_depth(), depth + 1);
    let project = v(&t).ed.project();
    let units: Vec<_> = project
        .assignments
        .iter()
        .filter(|a| a.task_uid == 10)
        .map(|a| (a.resource_uid, a.units))
        .collect();
    assert_eq!(units, [(1, 0.5), (2, 1.0), (3, 1.0)]);
    assert!(project.resources.iter().all(|r| !r.name.contains('[')));
    assert_eq!(row(&t), "Bob[50%], Alice, Carol");
}

// ---- the entry row below the last task (#145) ----

fn state(t: &DocTab, name: &str) -> ctlcore::json::Json {
    project_state(v(t), None)
        .into_iter()
        .chain(project_cell_state(v(t)))
        .find(|(k, _)| k == name)
        .unwrap()
        .1
}

#[test]
fn typing_into_the_entry_row_appends_one_task_as_one_undo_step() {
    use ctlcore::json::Json;
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    assert_eq!(state(&t, "cell_row"), Json::Num(3.));
    assert_eq!(state(&t, "selected_task"), Json::Num(3.));
    assert_eq!(state(&t, "selected_name"), Json::Str(String::new()));
    edit(&mut t, COL_NAME, "Design");
    assert_eq!(v(&t).cell.as_ref().unwrap().uid, None);
    assert_eq!(
        v(&t).ed.project().tasks.len(),
        3,
        "nothing until the commit"
    );
    key(&mut t, "enter");
    let tasks = &v(&t).ed.project().tasks;
    assert_eq!(tasks.len(), 4);
    let new = &tasks[3];
    // What Project makes of a typed blank row: `1 day?`.
    assert_eq!(
        (
            new.name.as_str(),
            new.duration_min,
            new.estimated,
            new.outline_level
        ),
        ("Design", 480, Some(true), 1)
    );
    assert!(!new.is_null);
    // Enter lands on the new entry row, ready for the next task.
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 4);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert!(t.dirty);
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(
        v(&t).ed.project().tasks.len(),
        3,
        "one Undo removes the task"
    );
    assert!(
        v(&t).on_entry_row(),
        "Undo keeps the cursor on the entry row"
    );
    assert_eq!(v(&t).cursor_row(), 3);
    apply_project_act(&mut t, ProjectAct::Redo);
    assert_eq!(v(&t).ed.project().tasks[3].name, "Design");
    assert!(v(&t).on_entry_row());
}

#[test]
fn tab_after_an_entry_row_commit_stays_on_the_new_task() {
    for (shift, col) in [(false, COL_DURATION), (true, COL_MODE)] {
        let mut t = tab();
        project_entry_click(&mut t, Some(COL_NAME), false);
        edit(&mut t, COL_NAME, "Design");
        project_input(
            &mut t,
            "tab",
            None,
            Modifiers {
                shift,
                ..Modifiers::default()
            },
        );
        assert!(v(&t).cell.is_none());
        assert!(!v(&t).on_entry_row());
        assert_eq!((v(&t).cursor_row(), v(&t).col), (3, col));
        assert_eq!(v(&t).ed.project().tasks[3].name, "Design");
    }
}

#[test]
fn every_column_of_the_entry_row_appends_a_task() {
    for (col, text, check) in [
        (COL_DURATION, "3d", "duration"),
        (COL_START, "2026-01-07", "start"),
        (COL_FINISH, "2026-01-09", "finish"),
        (COL_PREDECESSORS, "1", "predecessors"),
        (COL_RESOURCES, "Bob", "resources"),
    ] {
        let mut t = tab();
        project_entry_click(&mut t, Some(col), false);
        edit(&mut t, col, text);
        key(&mut t, "enter");
        let ed = &v(&t).ed;
        assert_eq!(ed.project().tasks.len(), 4, "{check}");
        let new = &ed.project().tasks[3];
        assert_eq!(project_row(ed, new)[col], text, "{check}");
        assert_eq!(ed.undo_depth(), 1, "{check}");
        assert!(v(&t).on_entry_row(), "{check}");
        if col == COL_START {
            assert_eq!(t.status.as_ref(), "Constraint set: SNET (was ASAP)");
        }
    }
}

#[test]
fn an_empty_or_rejected_entry_row_value_appends_nothing() {
    let mut t = tab();
    let before = v(&t).ed.project().clone();
    project_entry_click(&mut t, Some(COL_NAME), false);
    // F2 opens an empty edit; committing it unchanged appends nothing.
    key(&mut t, "f2");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none());
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!t.dirty);
    // ID stays read-only there too.
    vm(&mut t).col = COL_ID;
    project_input(&mut t, "x", Some("x"), Modifiers::default());
    assert!(v(&t).cell.is_none());
    assert_eq!(t.status.as_ref(), "ID is read-only");
    for (col, text) in [
        (COL_DURATION, "NaN"),
        (COL_DURATION, "-1d"),
        (COL_START, "2026-02-31"),
        (COL_PREDECESSORS, "99"),
    ] {
        project_entry_click(&mut t, Some(col), false);
        edit(&mut t, col, text);
        key(&mut t, "enter");
        let cell = v(&t)
            .cell
            .as_ref()
            .expect("a rejected value keeps the edit");
        assert_eq!(cell.buf, text);
        assert_eq!(cell.last_error.as_deref(), Some(t.status.as_ref()));
        assert!(v(&t).on_entry_row());
        key(&mut t, "escape");
        assert!(v(&t).cell.is_none());
        assert_eq!(v(&t).ed.project(), &before, "{text}: no trailing blank row");
        assert_eq!((v(&t).ed.undo_depth(), v(&t).ed.redo_depth()), (0, 0));
        assert!(!t.dirty);
    }
}

#[test]
fn a_new_plan_takes_its_first_task_from_the_entry_row() {
    use ctlcore::json::Json;
    let mut t = new_project_tab();
    assert!(v(&t).on_entry_row());
    assert_eq!(state(&t, "cell_row"), Json::Num(0.));
    for c in ["D", "e", "s", "i", "g", "n"] {
        project_input(&mut t, c, Some(c), Modifiers::default());
    }
    key(&mut t, "enter");
    let tasks = &v(&t).ed.project().tasks;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name, "Design");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!(v(&t).cursor_row(), 1);
    assert!(t.dirty);
}

#[test]
fn enter_after_editing_the_last_task_goes_to_the_entry_row() {
    let mut t = tab();
    key(&mut t, "end");
    edit(&mut t, COL_NAME, "Last");
    key(&mut t, "enter");
    assert_eq!(v(&t).ed.project().tasks[2].name, "Last");
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 3);
    // Up goes back to the last task; a click on a task row leaves the entry row.
    key(&mut t, "up");
    assert_eq!(v(&t).cursor_row(), 2);
    project_entry_click(&mut t, Some(COL_DURATION), false);
    project_cell_click(&mut t, 0, Some(COL_NAME), false);
    assert!(!v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 0);
}

#[test]
fn clicking_the_entry_row_while_editing_it_lands_on_the_new_task() {
    // Single click: the clicked cell now belongs to the task the commit made.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    edit(&mut t, COL_NAME, "Design");
    project_entry_click(&mut t, Some(COL_DURATION), false);
    assert_eq!(v(&t).ed.project().tasks.len(), 4);
    assert!(!v(&t).on_entry_row());
    assert_eq!((v(&t).cursor_row(), v(&t).col), (3, COL_DURATION));
    edit(&mut t, COL_DURATION, "3d");
    key(&mut t, "enter");
    let tasks = &v(&t).ed.project().tasks;
    assert_eq!(tasks.len(), 4, "no second, unnamed task");
    assert_eq!(
        (tasks[3].name.as_str(), tasks[3].duration_min),
        ("Design", 1440)
    );
    assert_eq!(v(&t).cursor_row(), 4);

    // Double click opens the new task's cell.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    edit(&mut t, COL_NAME, "Design");
    project_entry_click(&mut t, Some(COL_DURATION), true);
    let cell = v(&t).cell.as_ref().expect("the double click opens an edit");
    assert_eq!(
        (cell.uid, cell.col),
        (Some(v(&t).ed.project().tasks[3].uid), COL_DURATION)
    );

    // Outside a cell of that row (its chart side), too.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    edit(&mut t, COL_NAME, "Design");
    project_entry_click(&mut t, None, false);
    assert_eq!((v(&t).cursor_row(), v(&t).col), (3, COL_NAME));

    // A click below the entry row goes to the new entry row.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    edit(&mut t, COL_NAME, "Design");
    project_below_click(&mut t);
    assert_eq!(v(&t).ed.project().tasks.len(), 4);
    assert!(v(&t).on_entry_row());
    assert_eq!(v(&t).cursor_row(), 4);

    // An empty entry edit appends nothing, so the click stays on the entry row.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    key(&mut t, "f2");
    project_entry_click(&mut t, Some(COL_DURATION), false);
    assert!(v(&t).on_entry_row());
    assert_eq!((v(&t).cursor_row(), v(&t).col), (3, COL_DURATION));
}

// ---- the Task Mode column (#121) ----

#[test]
fn task_mode_accepts_projects_names_and_their_starts() {
    for (text, manual) in [
        ("Manually Scheduled", true),
        ("m", true),
        ("MAN", true),
        (" manually ", true),
        ("Auto Scheduled", false),
        ("a", false),
        ("Auto", false),
    ] {
        assert_eq!(parse_task_mode(text), Ok(manual), "{text}");
    }
    for text in ["", "  ", "x", "manual mode", "automatic"] {
        assert!(parse_task_mode(text).is_err(), "{text}");
    }
}

#[test]
fn the_task_mode_cell_shows_and_switches_the_mode_as_one_step() {
    let mut t = tab();
    let mode =
        |t: &DocTab| project_row(&v(t).ed, v(t).ed.project().task(10).unwrap())[COL_MODE].clone();
    assert_eq!(COLUMNS[COL_MODE], "Task Mode");
    assert_eq!(mode(&t), "Auto Scheduled");
    let start = v(&t).ed.disp_start(10);
    vm(&mut t).col = COL_MODE;
    key(&mut t, "f2");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "Auto Scheduled");
    key(&mut t, "escape");
    edit(&mut t, COL_MODE, "m");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(mode(&t), "Manually Scheduled");
    let task = v(&t).ed.project().task(10).unwrap();
    assert!(task.manual);
    assert_eq!(task.manual_start, start);
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert!(t.dirty);
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_MODE, "auto");
    key(&mut t, "enter");
    assert_eq!(mode(&t), "Auto Scheduled");
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(mode(&t), "Manually Scheduled");
}

#[test]
fn an_invalid_task_mode_keeps_the_edit_open_and_changes_nothing() {
    let mut t = tab();
    let before = v(&t).ed.project().clone();
    edit(&mut t, COL_MODE, "x");
    key(&mut t, "enter");
    let cell = v(&t).cell.as_ref().expect("the edit stays open");
    assert_eq!(cell.buf, "x");
    assert_eq!(cell.last_error.as_deref(), Some(t.status.as_ref()));
    assert!(t.status.contains("Task Mode"));
    assert_eq!(v(&t).ed.project(), &before);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!t.dirty);
}

#[test]
fn a_summary_and_the_entry_row_take_a_task_mode() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_MODE, "m");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    let summary = v(&t).ed.project().task(10).unwrap();
    assert!(summary.summary && summary.manual);
    // The entry row appends a task with the typed mode.
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_MODE), false);
    edit(&mut t, COL_MODE, "Manually Scheduled");
    key(&mut t, "enter");
    let ed = &v(&t).ed;
    assert_eq!(ed.project().tasks.len(), 4);
    assert!(ed.project().tasks[3].manual);
    assert_eq!(
        project_row(ed, &ed.project().tasks[3])[COL_MODE],
        "Manually Scheduled"
    );
    assert_eq!(ed.undo_depth(), 1);
}
