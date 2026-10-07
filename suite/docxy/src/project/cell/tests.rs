use super::*;
use core::prelude::v1::test;
use projcore::{ConstraintType, DateTime};

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
/// Project's Ctrl+Up/Down row jumps.
fn ctrl_key(t: &mut DocTab, key: &str) {
    let m = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    assert_eq!(project_input(t, key, None, m), None);
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
    ctrl_key(&mut t, "down");
    assert_eq!(v(&t).ed.sel(), 2);
    ctrl_key(&mut t, "up");
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
fn a_duration_shows_reopens_and_commits_in_its_own_unit() {
    let mut t = tab();
    let format = |t: &DocTab| v(t).ed.project().task(10).unwrap().duration_format;
    let shown = |t: &DocTab| {
        let ed = &v(t).ed;
        let task = ed.project().task(10).unwrap();
        (
            project_row(ed, task)[COL_DURATION].clone(),
            cell_edit_text(ed, task, COL_DURATION),
        )
    };
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_DURATION, "1.5w");
    key(&mut t, "enter");
    assert_eq!(format(&t), Some(9));
    assert_eq!(shown(&t), ("1.5w".into(), "1.5w".into()));
    // Reopening and committing keeps the unit, and is no edit.
    let depth = v(&t).ed.undo_depth();
    for (duration, format_before, row) in [
        ("1.5w", Some(9), "1.5w"),
        ("4h", Some(5), "4h"),
        ("0.5d", None, "0.5d"),
        ("1w?", Some(9), "1w?"),
    ] {
        vm(&mut t).ed.select(0);
        edit(&mut t, COL_DURATION, duration);
        key(&mut t, "enter");
        assert_eq!((format(&t), shown(&t).0.as_str()), (format_before, row));
        let depth = v(&t).ed.undo_depth();
        vm(&mut t).ed.select(0);
        vm(&mut t).col = COL_DURATION;
        key(&mut t, "f2");
        key(&mut t, "enter");
        assert_eq!(format(&t), format_before, "{duration}");
        assert_eq!(v(&t).ed.undo_depth(), depth, "{duration}");
    }
    assert!(v(&t).ed.undo_depth() > depth);
    // Ctrl+Delete gives a new task's day, in days.
    reset_cell(&mut vm(&mut t).ed, 10, COL_DURATION).unwrap();
    assert_eq!(format(&t), None);
    assert_eq!(shown(&t), ("1d?".into(), "1d?".into()));
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
    // The same duration in the same unit; `8h` would switch it to hours.
    edit(&mut t, COL_DURATION, "1d");
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
        apply_project_act(&mut t, ProjectAct::Constraint);
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
    apply_project_act(&mut t, ProjectAct::MoveTask);
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
fn a_manual_summary_s_dates_and_duration_are_editable() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    vm(&mut t).ed.set_manual(10, true).unwrap();
    vm(&mut t).ed.select(0);
    // Its duration cell opens on the span it shows, not the stored one.
    vm(&mut t).ed.set_duration(10, "2d").unwrap();
    vm(&mut t).col = COL_DURATION;
    vm(&mut t).open_cell(None).unwrap();
    assert_eq!(
        v(&t).cell.as_ref().unwrap().initial,
        format_duration_exact(960, v(&t).ed.project(), None)
    );
    vm(&mut t).cell = None;
    edit(&mut t, COL_DURATION, "3d");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.disp_duration_min(10), Some(3 * 480));
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_FINISH, "2026-01-09");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    let ed = &v(&t).ed;
    assert_eq!(
        ed.disp_finish(10),
        Some(DateTime::from_ymd_hm(2026, 1, 9, 17, 0))
    );
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_START, "2026-01-12");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    let ed = &v(&t).ed;
    assert_eq!(
        (ed.disp_start(10), ed.disp_finish(10)),
        (
            Some(DateTime::from_ymd_hm(2026, 1, 12, 8, 0)),
            Some(DateTime::from_ymd_hm(2026, 1, 16, 17, 0))
        )
    );
    // Its subtask starts no earlier than it does.
    assert_eq!(
        ed.disp_start(20),
        Some(DateTime::from_ymd_hm(2026, 1, 12, 8, 0))
    );
    assert_eq!(ed.undo_depth(), 6);
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
fn cell_edit_paste_inserts_at_the_caret_and_moves_it() {
    let mut c = CellEdit {
        last_error: None,
        uid: Some(1),
        col: COL_NAME,
        initial: "ac".into(),
        buf: "ac".into(),
        caret: 1,
    };
    c.paste("b");
    assert_eq!(c.buf, "abc");
    assert_eq!(c.caret, 2);
    // The caret is a UTF-8 byte offset: it advances by the inserted bytes.
    c.paste("é");
    assert_eq!(c.buf, "abéc");
    assert_eq!(c.caret, 4);
}

#[test]
fn cell_edit_paste_spaces_tabs_and_line_breaks_and_drops_the_trailing_ones() {
    let mut c = CellEdit {
        last_error: None,
        uid: Some(1),
        col: COL_NAME,
        initial: String::new(),
        buf: String::new(),
        caret: 0,
    };
    // As a copied cell's text: the trailing line break is dropped and every
    // remaining tab or line break becomes a space.
    c.paste("x\ny\tz\r\n");
    assert_eq!(c.buf, "x y z");
    assert_eq!(c.caret, 5);
    // An empty clipboard pastes nothing.
    c.paste("");
    assert_eq!(c.buf, "x y z");
    assert_eq!(c.caret, 5);
}

#[test]
fn cell_edit_cut_takes_the_whole_buffer_and_empties_it() {
    let mut c = CellEdit {
        last_error: None,
        uid: Some(1),
        col: COL_NAME,
        initial: "Task 2".into(),
        buf: "Task 2".into(),
        caret: 3,
    };
    assert_eq!(c.cut(), "Task 2");
    assert!(c.buf.is_empty());
    assert_eq!(c.caret, 0);
    // The edit stays usable: cutting again is empty, pasting re-fills.
    assert!(c.cut().is_empty());
    c.paste("a");
    assert_eq!(c.buf, "a");
    assert_eq!(c.caret, 1);
}

#[test]
fn in_cell_cut_and_paste_touch_the_project_only_on_commit() {
    let mut t = tab();
    vm(&mut t).open_cell(None).unwrap();
    vm(&mut t).cell.as_mut().unwrap().cut();
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "enter commits the emptied buffer");
    assert_eq!(v(&t).ed.project().task(10).unwrap().name, "");
    assert_eq!(v(&t).ed.undo_depth(), 1, "the clear is one undo step");
    assert!(v(&t).ed.dirty());
    assert!(vm(&mut t).ed.undo());
    assert_eq!(v(&t).ed.project().task(10).unwrap().name, "Task 1");
    // A pasted buffer escapes away: the project never saw it.
    let mut t = tab();
    vm(&mut t).open_cell(None).unwrap();
    vm(&mut t).cell.as_mut().unwrap().cut();
    vm(&mut t).cell.as_mut().unwrap().paste("renamed\r\n");
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "renamed");
    key(&mut t, "escape");
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).ed.project().task(10).unwrap().name, "Task 1");
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!v(&t).ed.dirty());
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

#[test]
fn resource_names_cell_keeps_a_material_label_with_a_comma() {
    let mut t = tab();
    let mut p = v(&t).ed.project().clone();
    p.resources.push(projcore::model::Resource {
        uid: 1,
        id: 1,
        name: "Cement".into(),
        kind: projcore::model::ResourceType::Material,
        material_label: Some("bags, 50 lb".into()),
        ..Default::default()
    });
    vm(&mut t).ed.replace_project(p);
    vm(&mut t)
        .ed
        .set_resources(10, &["Cement[5 bags, 50 lb]".into()])
        .unwrap();
    let row = |t: &DocTab| {
        project_row(&v(t).ed, v(t).ed.project().task(10).unwrap())[COL_RESOURCES].clone()
    };
    assert_eq!(row(&t), "Cement[5 bags, 50 lb]");
    let depth = v(&t).ed.undo_depth();
    vm(&mut t).col = COL_RESOURCES;
    // Committing the shown text is a no-op.
    key(&mut t, "f2");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.undo_depth(), depth);
    // Appending a name keeps the material and stages no split-off names.
    // (Enter moved the selection down a row.)
    vm(&mut t).ed.select(0);
    key(&mut t, "f2");
    project_input(&mut t, "text", Some(", Alice"), Modifiers::default());
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
    assert_eq!(units, [(1, 5.0), (2, 1.0)]);
    assert_eq!(project.resources.len(), 2);
    assert_eq!(row(&t), "Cement[5 bags, 50 lb], Alice");
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
    ctrl_key(&mut t, "down");
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

#[test]
fn retyping_a_predecessors_cell_keeps_a_link_shown_in_a_fallback_unit() {
    // #104 r1: a working-month lag shows in days. Retyping the cell with
    // another link added keeps its format; only the new link takes its own.
    let lag = |uid, lag, code| projcore::Predecessor {
        uid,
        link: projcore::LinkType::FinishStart,
        lag,
        lag_format: projcore::LagFormat::from_code(code).unwrap(),
        ..projcore::Predecessor::fs(uid)
    };
    let mut t = tab();
    let month = lag(20, 20 * 480, 11);
    vm(&mut t)
        .ed
        .set_predecessors(10, vec![month.clone()])
        .unwrap();
    edit(&mut t, COL_PREDECESSORS, "2FS+20d, 3FS+2ed");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(
        v(&t).ed.project().task(10).unwrap().predecessors,
        vec![month, lag(30, 2880, 8)]
    );
}

// ---- estimated durations (#159) ----

fn duration_text(t: &DocTab, uid: i32) -> String {
    let ed = &v(t).ed;
    project_row(ed, ed.project().task(uid).unwrap())[COL_DURATION].clone()
}

#[test]
fn an_estimated_duration_shows_and_reopens_with_a_question_mark() {
    let mut p = v(&tab()).ed.project().clone();
    p.tasks[0].estimated = Some(true);
    p.tasks[1].duration_min = 1200;
    p.tasks[1].estimated = Some(true);
    p.tasks[2].estimated = Some(false);
    let mut t = project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    );
    assert_eq!(duration_text(&t, 10), "1d?");
    assert_eq!(duration_text(&t, 20), "2.5d?");
    assert_eq!(duration_text(&t, 30), "1d");
    // The cell opens as it shows; committing it unchanged is no edit.
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_DURATION;
    vm(&mut t).open_cell(None).unwrap();
    assert_eq!(v(&t).cell.as_ref().unwrap().initial, "1d?");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!((v(&t).ed.undo_depth(), t.dirty), (0, false));
    // Retyping it without `?` commits the estimate.
    vm(&mut t).ed.select(0);
    edit(&mut t, COL_DURATION, "1d");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.project().task(10).unwrap().estimated, Some(false));
    assert_eq!(duration_text(&t, 10), "1d");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    // And `?` marks one.
    vm(&mut t).ed.select(2);
    edit(&mut t, COL_DURATION, "3 days?");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(duration_text(&t, 30), "3d?");
}

#[test]
fn the_entry_row_takes_an_estimated_duration() {
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_DURATION), false);
    edit(&mut t, COL_DURATION, "2 days?");
    key(&mut t, "enter");
    let ed = &v(&t).ed;
    let new = &ed.project().tasks[3];
    assert_eq!((new.duration_min, new.estimated), (960, Some(true)));
    assert_eq!(project_row(ed, new)[COL_DURATION], "2d?");
    assert_eq!(ed.undo_depth(), 1);
}

#[test]
fn a_summary_shows_the_estimate_of_its_subtasks() {
    let mut p = v(&tab()).ed.project().clone();
    p.tasks[0].outline_level = 1;
    p.tasks[1].outline_level = 2;
    p.tasks[2].outline_level = 2;
    p.tasks[2].estimated = Some(true);
    let t = project_tab(
        "test.yppx".into(),
        None,
        Surface::Project(ProjectView::new(p, false)),
        false,
        "loaded".into(),
    );
    assert!(
        duration_text(&t, 10).ends_with("d?"),
        "{}",
        duration_text(&t, 10)
    );
    assert_eq!(duration_text(&t, 20), "1d");
}

fn shift_key(t: &mut DocTab, key: &str) {
    let m = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    project_input(t, key, None, m);
}

#[test]
fn up_or_down_commits_a_new_plan_entry_row_edit_as_enter_does() {
    for (arrow, row) in [("down", 1), ("up", 0)] {
        let mut t = new_project_tab();
        let col = v(&t).col;
        project_input(&mut t, "a", Some("A"), Modifiers::default());
        key(&mut t, arrow);
        let tasks = &v(&t).ed.project().tasks;
        assert_eq!(tasks.len(), 1, "{arrow}");
        assert_eq!(
            (
                tasks[0].name.as_str(),
                tasks[0].duration_min,
                tasks[0].estimated
            ),
            ("A", 480, Some(true)),
            "{arrow}: 1d? as with Enter"
        );
        assert!(v(&t).cell.is_none(), "{arrow}");
        assert_eq!(v(&t).ed.undo_depth(), 1, "{arrow}");
        assert_eq!((v(&t).cursor_row(), v(&t).col), (row, col), "{arrow}");
        // Down lands on the entry row below; Up has no row above task A.
        assert_eq!(v(&t).on_entry_row(), arrow == "down", "{arrow}");
    }
}

#[test]
fn up_after_appending_from_the_entry_row_goes_to_the_row_above_the_new_task() {
    let mut t = tab();
    project_entry_click(&mut t, Some(COL_NAME), false);
    edit(&mut t, COL_NAME, "Design");
    key(&mut t, "up");
    assert_eq!(v(&t).ed.project().tasks.len(), 4);
    assert_eq!(v(&t).ed.project().tasks[3].name, "Design");
    assert!(v(&t).cell.is_none());
    assert!(!v(&t).on_entry_row());
    assert_eq!((v(&t).cursor_row(), v(&t).col), (2, COL_NAME));
}

#[test]
fn up_or_down_commits_a_task_edit_and_moves_one_row() {
    for (arrow, row, shift) in [("down", 2, false), ("up", 0, false), ("down", 2, true)] {
        let mut t = tab();
        key(&mut t, "down");
        edit(&mut t, COL_NAME, "Renamed");
        if shift {
            shift_key(&mut t, arrow);
        } else {
            key(&mut t, arrow);
        }
        assert!(v(&t).cell.is_none(), "{arrow}");
        assert_eq!(v(&t).ed.project().tasks[1].name, "Renamed", "{arrow}");
        assert_eq!(v(&t).ed.undo_depth(), 1, "{arrow}");
        assert_eq!((v(&t).cursor_row(), v(&t).col), (row, COL_NAME), "{arrow}");
        assert!(t.dirty);
    }
}

#[test]
fn a_rejected_value_keeps_the_editor_open_on_up_or_down() {
    let mut enter = tab();
    edit(&mut enter, COL_DURATION, "abc");
    key(&mut enter, "enter");
    assert!(
        enter.status.contains("Invalid duration"),
        "{}",
        enter.status
    );
    for arrow in ["down", "up"] {
        let mut t = tab();
        key(&mut t, "down");
        edit(&mut t, COL_DURATION, "abc");
        key(&mut t, arrow);
        assert_eq!(v(&t).cell.as_ref().map(|c| c.buf.as_str()), Some("abc"));
        assert_eq!(t.status, enter.status, "{arrow}: the error Enter gives");
        assert_eq!(v(&t).cursor_row(), 1, "{arrow}");
        assert_eq!(v(&t).ed.undo_depth(), 0, "{arrow}");
        assert!(!t.dirty);
    }
}

#[test]
fn shift_enter_commits_and_moves_up() {
    let mut t = tab();
    key(&mut t, "down");
    edit(&mut t, COL_NAME, "Renamed");
    shift_key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.project().tasks[1].name, "Renamed");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!((v(&t).cursor_row(), v(&t).col), (0, COL_NAME));
    assert!(t.dirty);
}

#[test]
fn shift_enter_on_first_row_commits_and_stays() {
    let mut t = tab();
    edit(&mut t, COL_NAME, "Renamed");
    shift_key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.project().tasks[0].name, "Renamed");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!((v(&t).cursor_row(), v(&t).col), (0, COL_NAME));
}

#[test]
fn shift_enter_on_new_entry_row_matches_up() {
    let mut t = new_project_tab();
    let col = v(&t).col;
    project_input(&mut t, "a", Some("A"), Modifiers::default());
    shift_key(&mut t, "enter");
    let tasks = &v(&t).ed.project().tasks;
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        (
            tasks[0].name.as_str(),
            tasks[0].duration_min,
            tasks[0].estimated
        ),
        ("A", 480, Some(true))
    );
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!((v(&t).cursor_row(), v(&t).col), (0, col));
    assert!(!v(&t).on_entry_row());
}

#[test]
fn shift_enter_with_invalid_value_keeps_the_edit() {
    let mut t = tab();
    key(&mut t, "down");
    edit(&mut t, COL_DURATION, "abc");
    shift_key(&mut t, "enter");
    assert_eq!(v(&t).cell.as_ref().map(|c| c.buf.as_str()), Some("abc"));
    assert!(t.status.contains("Invalid duration"), "{}", t.status);
    assert_eq!(v(&t).cursor_row(), 1);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert!(!t.dirty);
}

#[test]
fn enter_still_moves_down() {
    let mut t = tab();
    edit(&mut t, COL_NAME, "Renamed");
    key(&mut t, "enter");
    assert!(v(&t).cell.is_none(), "{}", t.status);
    assert_eq!(v(&t).ed.project().tasks[0].name, "Renamed");
    assert_eq!(v(&t).ed.undo_depth(), 1);
    assert_eq!((v(&t).cursor_row(), v(&t).col), (1, COL_NAME));
    assert!(t.dirty);
}

#[test]
fn up_or_down_from_an_unchanged_editor_closes_it_and_moves() {
    // F2 on a task, then Down: no undo step, one row down.
    let mut t = tab();
    key(&mut t, "f2");
    key(&mut t, "down");
    assert!(v(&t).cell.is_none());
    assert_eq!((v(&t).cursor_row(), v(&t).ed.undo_depth()), (1, 0));
    assert!(!t.dirty);
    // The last task's editor: Down goes to the entry row.
    ctrl_key(&mut t, "down");
    key(&mut t, "f2");
    key(&mut t, "down");
    assert!(v(&t).cell.is_none());
    assert!(v(&t).on_entry_row());
    // The entry row's editor with nothing typed: Up goes to the last task.
    key(&mut t, "f2");
    assert!(v(&t).cell.is_some());
    key(&mut t, "up");
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).cursor_row(), 2);
    assert_eq!(v(&t).ed.project().tasks.len(), 3);
    assert_eq!(v(&t).ed.undo_depth(), 0);
    // An empty plan's entry row: Up has nowhere to go and appends nothing.
    let mut t = new_project_tab();
    key(&mut t, "f2");
    key(&mut t, "up");
    assert!(v(&t).cell.is_none());
    assert!(v(&t).on_entry_row());
    assert!(v(&t).ed.project().tasks.is_empty());
}

#[test]
fn caret_keys_stay_in_the_editor() {
    let mut t = tab();
    edit(&mut t, COL_NAME, "ab");
    key(&mut t, "left");
    assert_eq!(v(&t).cell.as_ref().unwrap().caret, 1);
    key(&mut t, "home");
    assert_eq!(v(&t).cell.as_ref().unwrap().caret, 0);
    key(&mut t, "end");
    key(&mut t, "backspace");
    let cell = v(&t).cell.as_ref().unwrap();
    assert_eq!((cell.buf.as_str(), cell.caret), ("a", 1));
    assert_eq!(v(&t).ed.undo_depth(), 0);
    assert_eq!(v(&t).cursor_row(), 0);
}

#[test]
fn f11_commits_the_cell_before_the_new_project_and_an_invalid_cell_stays() {
    let mut t = tab();
    edit(&mut t, COL_NAME, "Named");
    assert_eq!(
        project_input(&mut t, "f11", None, Modifiers::default()),
        Some(ProjectAct::NewProject)
    );
    assert!(v(&t).cell.is_none());
    assert_eq!(v(&t).ed.project().task(10).unwrap().name, "Named");
    let mut t = tab();
    let before = v(&t).ed.project().clone();
    edit(&mut t, COL_DURATION, "invalid");
    assert_eq!(
        project_input(&mut t, "f11", None, Modifiers::default()),
        None
    );
    assert_eq!(v(&t).cell.as_ref().unwrap().buf, "invalid");
    assert_eq!(v(&t).ed.project(), &before);
    // With a modifier, F11 is just another key to a valid cell: no commit.
    let with = |shift, control, alt| Modifiers {
        shift,
        control,
        alt,
        ..Modifiers::default()
    };
    for m in [
        with(true, false, false),
        with(false, true, false),
        with(false, false, true),
        with(true, true, false),
    ] {
        let mut t = tab();
        edit(&mut t, COL_NAME, "Named");
        assert_eq!(project_input(&mut t, "f11", None, m), None, "{m:?}");
        assert_eq!(v(&t).cell.as_ref().unwrap().buf, "Named", "{m:?}");
        assert_eq!(v(&t).ed.project().task(10).unwrap().name, "Task 1");
    }
}

// ---- Range selection (#560) -------------------------------------------------

fn shift(t: &mut DocTab, key_name: &str) {
    let m = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(project_input(t, key_name, None, m), None);
}

/// The selection as (uids, first col, last col) for a straight assertion.
fn sel(t: &DocTab) -> Option<(Vec<i32>, usize, usize)> {
    v(t).selection()
        .map(|s| (s.uids, *s.cols.start(), *s.cols.end()))
}

#[test]
fn shift_arrows_extend_a_rectangle_and_plain_arrows_clear_it() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
    shift(&mut t, "right");
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_DURATION)));
    // A plain arrow moves the cursor and clears the range.
    key(&mut t, "down");
    assert_eq!(sel(&t), None);
    assert_eq!(
        (v(&t).ed.selected_uid(), v(&t).col),
        (Some(30), COL_DURATION)
    );
}

#[test]
fn shift_down_stops_on_the_last_task_and_never_selects_the_entry_row() {
    let mut t = tab();
    vm(&mut t).ed.select(2);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    // The last task stays put: anchor on cursor means no range.
    assert_eq!(sel(&t), None);
    assert!(!v(&t).on_entry_row());
    // On the entry row Shift+arrows do nothing at all.
    vm(&mut t).enter_entry_row();
    shift(&mut t, "up");
    shift(&mut t, "down");
    assert_eq!(sel(&t), None);
    assert!(v(&t).on_entry_row());
    // Back on the grid, Shift+Up from the first task stays on it.
    vm(&mut t).ed.select(0);
    shift(&mut t, "up");
    assert_eq!(sel(&t), None);
}

#[test]
fn selection_skips_rows_hidden_under_a_collapsed_summary() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    assert!(vm(&mut t).ed.set_collapsed(10, true).is_ok());
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    // The hidden row 20 is not in the range; the box is the two shown rows.
    assert_eq!(sel(&t), Some((vec![10, 30], COL_NAME, COL_NAME)));
    assert_eq!(v(&t).ed.selected_uid(), Some(30));
}

#[test]
fn selection_is_none_when_the_anchor_task_is_deleted() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    assert!(sel(&t).is_some());
    // An edit that removes the anchor task (an agent's task.del, an undo):
    // no stale highlight, no panic.
    vm(&mut t).ed.delete_task(10).unwrap();
    assert_eq!(sel(&t), None);
    assert!(vm(&mut t).ed.undo());
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
}

#[test]
fn shrinking_back_to_the_anchor_leaves_no_range() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    shift(&mut t, "up");
    assert_eq!(sel(&t), None);
    // Extending again anchors where the cursor was, not where it started.
    shift(&mut t, "down");
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
}

#[test]
fn press_drag_release_selects_the_rectangle() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    assert!(v(&t).dragging);
    assert!(!v(&t).drag_made_range, "the press cell is not a drag yet");
    assert!(project_cell_drag_over(&mut t, 1, COL_NAME));
    assert!(project_cell_drag_over(&mut t, 2, COL_DURATION));
    assert!(
        !project_cell_drag_over(&mut t, 2, COL_DURATION),
        "no move, no redraw"
    );
    project_cell_release(&mut t);
    assert!(!v(&t).dragging);
    assert_eq!(sel(&t), Some((vec![10, 20, 30], COL_NAME, COL_DURATION)));
    // A drag onto the entry row stops on the last task.
    project_cell_press(&mut t, 0, COL_NAME, false);
    assert!(!project_cell_drag_over(&mut t, 3, COL_NAME));
    project_cell_release(&mut t);
    assert_eq!(sel(&t), None, "no travel: the press cleared the range");
}

#[test]
fn a_click_after_a_drag_keeps_the_range() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 2, COL_NAME);
    project_cell_release(&mut t);
    // gpui delivers the release-click on the cell the drag ended on (and its
    // row): neither may clear the gesture's range.
    project_cell_click(&mut t, 2, Some(COL_NAME), false);
    assert_eq!(sel(&t), Some((vec![10, 20, 30], COL_NAME, COL_NAME)));
    // The next plain click runs normally and clears it.
    project_cell_press(&mut t, 1, COL_NAME, false);
    project_cell_release(&mut t);
    project_cell_click(&mut t, 1, Some(COL_NAME), false);
    assert_eq!(sel(&t), None);
    assert_eq!((v(&t).ed.selected_uid(), v(&t).col), (Some(20), COL_NAME));
}

#[test]
fn a_drag_released_below_the_table_keeps_the_range() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 1, COL_NAME);
    // The release lands below the table: the body's on_click is swallowed.
    project_cell_release(&mut t);
    project_below_click(&mut t);
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
    // The window-level release ends the gesture and is idempotent.
    project_cell_release(&mut t);
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
}

#[test]
fn a_plain_press_without_drag_clears_it() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    assert!(sel(&t).is_some());
    // Press and release on one cell: a click, the range does not survive.
    project_cell_press(&mut t, 1, COL_DURATION, false);
    assert!(!v(&t).drag_made_range);
    project_cell_release(&mut t);
    project_cell_click(&mut t, 1, Some(COL_DURATION), false);
    assert_eq!(sel(&t), None);
}

#[test]
fn shift_click_extends_from_the_cursor() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    project_cell_press(&mut t, 2, COL_DURATION, true);
    // The release-click is swallowed, so the range stays.
    project_cell_release(&mut t);
    project_cell_click(&mut t, 2, Some(COL_DURATION), false);
    assert_eq!(sel(&t), Some((vec![10, 20, 30], COL_NAME, COL_DURATION)));
    // And a second Shift+click re-anchors at the cursor, as specified.
    project_cell_press(&mut t, 1, COL_NAME, true);
    project_cell_release(&mut t);
    project_cell_click(&mut t, 1, Some(COL_NAME), false);
    assert_eq!(sel(&t), Some((vec![20, 30], COL_NAME, COL_DURATION)));
}

#[test]
fn clicking_the_id_cell_selects_the_whole_row_and_keeps_the_cursor_on_id() {
    let mut t = tab();
    project_cell_press(&mut t, 1, COL_ID, false);
    assert!(v(&t).row_drag);
    project_cell_release(&mut t);
    // The single click on the ID cell selects the whole row (all 8 columns).
    project_cell_click(&mut t, 1, Some(COL_ID), false);
    let s = v(&t).selection().unwrap();
    assert_eq!(s.count(), COLUMN_COUNT);
    assert_eq!(s.uids, [20]);
    assert_eq!((v(&t).ed.selected_uid(), v(&t).col), (Some(20), COL_ID));
    // Dragging down from an ID cell selects whole rows.
    project_cell_press(&mut t, 0, COL_ID, false);
    assert!(project_cell_drag_over(&mut t, 2, COL_DURATION));
    project_cell_release(&mut t);
    let s = v(&t).selection().unwrap();
    assert_eq!(s.count(), 3 * COLUMN_COUNT);
    assert_eq!(s.uids, [10, 20, 30]);
    assert_eq!(v(&t).col, COL_ID, "the cursor column stays on ID");
    // A double-click on ID is not swallowed: the read-only status still shows.
    project_cell_press(&mut t, 0, COL_ID, false);
    project_cell_release(&mut t);
    project_cell_click(&mut t, 0, Some(COL_ID), true);
    assert_eq!(t.status.as_ref(), "ID is read-only");
}

#[test]
fn commands_and_typing_clear_the_range_but_commands_keep_it() {
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    // Typing opens the cell and clears the range.
    shift(&mut t, "down");
    assert!(sel(&t).is_some());
    project_input(&mut t, "x", Some("x"), Modifiers::default());
    assert_eq!(sel(&t), None);
    key(&mut t, "escape");
    // Escape with nothing open clears it too.
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    project_input(&mut t, "escape", None, Modifiers::default());
    assert_eq!(sel(&t), None);
    // Any other command clears it: Undo as the example.
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    apply_project_act(&mut t, ProjectAct::Undo);
    assert_eq!(sel(&t), None);
    // Link and Unlink keep it (Copy does too; Cut and Paste clear it
    // themselves, in clip.rs).
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    apply_project_act(&mut t, ProjectAct::AddLink);
    assert!(sel(&t).is_some(), "Link Tasks keeps the range");
    apply_project_act(&mut t, ProjectAct::UnlinkTasks);
    assert!(sel(&t).is_some(), "Unlink Tasks keeps the range");
}

#[test]
fn selection_is_part_of_the_harness_state() {
    use ctlcore::json::Json;
    let mut t = tab();
    assert_eq!(state(&t, "selection"), Json::Null);
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    shift(&mut t, "right");
    assert_eq!(state(&t, "selection"), Json::Num(4.));
}

#[test]
fn a_click_without_a_press_clears_the_range() {
    // The row's chart half and the ruled rows below reach cell_click with no
    // press before them; the range a Shift+arrow made does not survive.
    let mut t = tab();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    assert!(sel(&t).is_some());
    project_cell_click(&mut t, 2, None, false);
    assert_eq!(sel(&t), None);
    assert_eq!(v(&t).ed.selected_uid(), Some(30));
    project_below_click(&mut t);
    assert!(v(&t).on_entry_row());
    assert_eq!(sel(&t), None);
    // A Shift press on the entry row drops the old anchor too.
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    project_cell_press(&mut t, 3, COL_NAME, true);
    assert_eq!(sel(&t), None);
    assert!(v(&t).on_entry_row());
    project_cell_release(&mut t);
}

#[test]
fn a_drag_released_off_the_table_does_not_swallow_the_next_click() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 1, COL_NAME);
    // The release lands off the table (the split gutter, a strip, outside):
    // no click follows it, so the flag stands until the next press.
    project_cell_release(&mut t);
    assert!(v(&t).drag_made_range);
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
    // The next press of any kind — the suite-root mouse-down's capture phase
    // calls project_cell_press_reset, so a press on the chart half or below
    // the table does this too — spends the flag before its click runs.
    project_cell_press_reset(&mut t);
    assert!(!v(&t).drag_made_range);
    project_cell_click(&mut t, 2, Some(COL_NAME), false);
    assert_eq!(sel(&t), None, "the plain click clears what it did not make");
    assert_eq!((v(&t).ed.selected_uid(), v(&t).col), (Some(30), COL_NAME));
    // The same through a real cell press, which resets the flag itself.
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 1, COL_NAME);
    project_cell_release(&mut t);
    project_cell_press(&mut t, 2, COL_NAME, false);
    assert!(!v(&t).drag_made_range, "the press spent the flag");
    project_cell_click(&mut t, 2, Some(COL_NAME), false);
    assert_eq!(sel(&t), None);
}

#[test]
fn a_drag_released_over_the_table_keeps_the_flag_for_its_click() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 1, COL_NAME);
    // A release-click follows in this same mouse-up: the flag outlives the
    // release so the click is swallowed and the range stays.
    project_cell_release(&mut t);
    assert!(v(&t).drag_made_range);
    project_cell_click(&mut t, 1, Some(COL_NAME), false);
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
    assert!(!v(&t).drag_made_range, "the release-click spent the flag");
}

#[test]
fn shift_extension_reanchors_when_the_anchor_task_is_gone() {
    let mut t = tab();
    vm(&mut t).ed.add_task(None, "Fourth", 480, false).unwrap();
    let fourth = v(&t).ed.project().tasks[3].uid;
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    assert_eq!(sel(&t), Some((vec![10, 20], COL_NAME, COL_NAME)));
    // The anchor's task goes away (an agent's task.del): the stored anchor
    // resolves to nothing, and the next Shift+arrow re-anchors at the
    // cursor instead of selecting nothing forever.
    vm(&mut t).ed.delete_task(10).unwrap();
    shift(&mut t, "down");
    assert_eq!(
        sel(&t),
        Some((vec![30, fourth], COL_NAME, COL_NAME)),
        "re-anchored at the cursor, then extended"
    );
}

#[test]
fn a_bare_press_ends_a_drag_the_release_never_reached() {
    let mut t = tab();
    project_cell_press(&mut t, 0, COL_NAME, false);
    project_cell_drag_over(&mut t, 1, COL_NAME);
    // The release lands where even the window's element listeners never run
    // (the split gutter, outside the window): the drag stays armed, and
    // plain hover cannot extend it (the move gate). The next press of any
    // kind — the window-level mouse-down listener — disarms it, on cells and
    // bare chart-half/below-table presses alike.
    project_cell_press_reset(&mut t);
    assert!(!v(&t).dragging && !v(&t).row_drag);
    assert!(
        !project_cell_drag_over(&mut t, 2, COL_NAME),
        "no fresh press armed a drag"
    );
    // The bare press's release-click places the cursor and drops the range.
    project_cell_click(&mut t, 2, None, false);
    assert_eq!(sel(&t), None);
    assert_eq!((v(&t).ed.selected_uid(), v(&t).col), (Some(30), COL_NAME));
}

#[test]
fn collapsing_a_summary_drops_the_range() {
    let mut t = tab();
    vm(&mut t).ed.indent(20, 1).unwrap();
    vm(&mut t).ed.select(0);
    vm(&mut t).col = COL_NAME;
    shift(&mut t, "down");
    shift(&mut t, "down");
    assert_eq!(sel(&t), Some((vec![10, 20, 30], COL_NAME, COL_NAME)));
    // The glyph's click is a plain click: collapsing drops the range, and
    // expanding cannot revive it. The cursor stays on the task it was on.
    toggle_project_collapse(&mut t, 10);
    assert_eq!(sel(&t), None);
    assert_eq!(v(&t).ed.selected_uid(), Some(30));
    toggle_project_collapse(&mut t, 10);
    assert_eq!(sel(&t), None, "expanding does not revive the old anchor");
}

#[test]
fn the_window_level_gesture_end_reaches_every_project_tab() {
    let mut tabs = vec![tab(), tab()];
    project_cell_press(&mut tabs[0], 0, COL_NAME, false);
    project_cell_drag_over(&mut tabs[0], 1, COL_NAME);
    assert!(v(&tabs[0]).dragging);
    // A gesture can outlive its tab's activation (a tab switch mid-drag,
    // F11): the window-level press reset and release run over EVERY Project
    // tab, not only the active one.
    crate::project_cell_press_reset_all(&mut tabs);
    assert!(
        !v(&tabs[0]).dragging && !v(&tabs[0]).drag_made_range,
        "the reset reached the background tab's gesture"
    );
    project_cell_press(&mut tabs[0], 0, COL_NAME, false);
    project_cell_drag_over(&mut tabs[0], 1, COL_NAME);
    crate::project_cell_release_all(&mut tabs);
    assert!(
        !v(&tabs[0]).dragging,
        "the release ended the background drag"
    );
    assert!(
        sel(&tabs[0]).is_some(),
        "ending the drag does not clear the gesture's range"
    );
}
